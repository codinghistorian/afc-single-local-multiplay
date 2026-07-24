//! Crash-safe, bounded persistence for authority-confirmed replay files.
//!
//! Filenames contain protocol IDs only. Writes are staged and fsynced in the
//! destination directory, then published without replacing an existing replay.
//! Repeating the same result is idempotent; conflicting bytes fail closed.

use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use crate::network_protocol::MatchId;
use crate::replay::{AuthorityResultId, MAX_REPLAY_BYTES, Replay, ReplayError};

pub const REPLAY_FILE_EXTENSION: &str = "afcr";
pub const DEFAULT_REPLAY_RETENTION_FILES: usize = 32;
pub const DEFAULT_REPLAY_RETENTION_BYTES: u64 = 512 * 1_024 * 1_024;
pub const MAX_REPLAY_ARCHIVE_ENTRIES: usize = 128;
const TEMP_CREATE_ATTEMPTS: u64 = 32;
static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredReplay {
    pub path: PathBuf,
    pub encoded_bytes: usize,
    pub disposition: ReplaySaveDisposition,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplaySaveDisposition {
    Created,
    AlreadyPresent,
}

#[derive(Debug)]
pub enum ReplayArchiveError {
    Replay(ReplayError),
    Io {
        operation: &'static str,
        source: io::Error,
    },
    FileTooLarge {
        bytes: u64,
        maximum: usize,
    },
    ConflictingExistingReplay(PathBuf),
    InvalidRetentionPolicy,
    ArchiveEntryLimitExceeded,
    TemporaryNameExhausted,
}

impl fmt::Display for ReplayArchiveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Replay(error) => write!(formatter, "invalid replay archive payload: {error}"),
            Self::Io { operation, source } => {
                write!(formatter, "replay archive {operation} failed: {source}")
            }
            Self::FileTooLarge { bytes, maximum } => write!(
                formatter,
                "replay archive file is {bytes} bytes; maximum is {maximum}"
            ),
            Self::ConflictingExistingReplay(path) => write!(
                formatter,
                "replay result already exists with different bytes at {}",
                path.display()
            ),
            Self::InvalidRetentionPolicy => formatter.write_str("invalid replay retention policy"),
            Self::ArchiveEntryLimitExceeded => write!(
                formatter,
                "replay archive contains more than {MAX_REPLAY_ARCHIVE_ENTRIES} managed files"
            ),
            Self::TemporaryNameExhausted => {
                write!(
                    formatter,
                    "could not reserve a bounded replay temporary filename"
                )
            }
        }
    }
}

impl Error for ReplayArchiveError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Replay(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<ReplayError> for ReplayArchiveError {
    fn from(error: ReplayError) -> Self {
        Self::Replay(error)
    }
}

/// Authority replay directory. Exact match/result identities address files;
/// each save also performs one bounded scan of managed replay files to enforce
/// the configured retention limits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayArchive {
    root: PathBuf,
    policy: ReplayRetentionPolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplayRetentionPolicy {
    pub maximum_files: usize,
    pub maximum_bytes: u64,
}

impl Default for ReplayRetentionPolicy {
    fn default() -> Self {
        Self {
            maximum_files: DEFAULT_REPLAY_RETENTION_FILES,
            maximum_bytes: DEFAULT_REPLAY_RETENTION_BYTES,
        }
    }
}

impl ReplayArchive {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            policy: ReplayRetentionPolicy::default(),
        }
    }

    pub fn with_policy(
        root: impl Into<PathBuf>,
        policy: ReplayRetentionPolicy,
    ) -> Result<Self, ReplayArchiveError> {
        if policy.maximum_files == 0 || policy.maximum_bytes < MAX_REPLAY_BYTES as u64 {
            return Err(ReplayArchiveError::InvalidRetentionPolicy);
        }
        Ok(Self {
            root: root.into(),
            policy,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub const fn policy(&self) -> ReplayRetentionPolicy {
        self.policy
    }

    pub fn path_for(&self, match_id: MatchId, result_id: AuthorityResultId) -> PathBuf {
        self.root.join(replay_filename(match_id, result_id))
    }

    pub fn save(&self, replay: &Replay) -> Result<StoredReplay, ReplayArchiveError> {
        let encoded = replay.encode()?;
        let path = self.path_for(replay.header.match_id, replay.final_result.result_id);
        create_private_directory(&self.root)?;
        let (temporary_path, mut temporary) = self.reserve_temporary(&path)?;
        let mut cleanup = TemporaryCleanup::new(temporary_path.clone());
        temporary
            .write_all(&encoded)
            .map_err(|source| io_error("write temporary file", source))?;
        temporary
            .sync_all()
            .map_err(|source| io_error("sync temporary file", source))?;
        drop(temporary);

        let disposition = match fs::hard_link(&temporary_path, &path) {
            Ok(()) => ReplaySaveDisposition::Created,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let retained = read_bounded(&path)?;
                if retained != encoded {
                    return Err(ReplayArchiveError::ConflictingExistingReplay(path));
                }
                ReplaySaveDisposition::AlreadyPresent
            }
            Err(source) => return Err(io_error("publish temporary file", source)),
        };
        fs::remove_file(&temporary_path)
            .map_err(|source| io_error("remove temporary file", source))?;
        cleanup.disarm();
        restrict_private_file(&path)?;
        File::open(&path)
            .and_then(|file| file.sync_all())
            .map_err(|source| io_error("sync published file", source))?;
        sync_directory(&self.root)?;
        self.enforce_retention(&path)?;

        Ok(StoredReplay {
            path,
            encoded_bytes: encoded.len(),
            disposition,
        })
    }

    pub fn load(
        &self,
        match_id: MatchId,
        result_id: AuthorityResultId,
    ) -> Result<Replay, ReplayArchiveError> {
        let bytes = read_bounded(&self.path_for(match_id, result_id))?;
        Replay::decode(&bytes).map_err(Into::into)
    }

    fn reserve_temporary(&self, final_path: &Path) -> Result<(PathBuf, File), ReplayArchiveError> {
        let stem = final_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("replay");
        for _ in 0..TEMP_CREATE_ATTEMPTS {
            let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
            let path = self
                .root
                .join(format!(".{stem}.{}.{}.tmp", std::process::id(), sequence));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            match options.open(&path) {
                Ok(file) => return Ok((path, file)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => return Err(io_error("create temporary file", source)),
            }
        }
        Err(ReplayArchiveError::TemporaryNameExhausted)
    }

    fn enforce_retention(&self, retained_path: &Path) -> Result<(), ReplayArchiveError> {
        let mut entries = Vec::new();
        for entry in fs::read_dir(&self.root)
            .map_err(|source| io_error("read retention directory", source))?
        {
            let entry = entry.map_err(|source| io_error("read retention entry", source))?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str())
                != Some(REPLAY_FILE_EXTENSION)
            {
                continue;
            }
            if entries.len() == MAX_REPLAY_ARCHIVE_ENTRIES {
                return Err(ReplayArchiveError::ArchiveEntryLimitExceeded);
            }
            let metadata = entry
                .metadata()
                .map_err(|source| io_error("read retention metadata", source))?;
            if !metadata.is_file() {
                continue;
            }
            entries.push((path, metadata.len(), metadata.modified().ok()));
        }

        entries.sort_by(|left, right| {
            let left_retained = left.0 == retained_path;
            let right_retained = right.0 == retained_path;
            left_retained.cmp(&right_retained).then_with(|| {
                left.2
                    .cmp(&right.2)
                    .then_with(|| left.0.file_name().cmp(&right.0.file_name()))
            })
        });
        let mut bytes = entries
            .iter()
            .fold(0_u64, |total, entry| total.saturating_add(entry.1));
        let mut files = entries.len();
        for (path, size, _) in entries {
            if files <= self.policy.maximum_files && bytes <= self.policy.maximum_bytes {
                break;
            }
            if path == retained_path {
                continue;
            }
            fs::remove_file(&path).map_err(|source| io_error("prune replay", source))?;
            files = files.saturating_sub(1);
            bytes = bytes.saturating_sub(size);
        }
        if files > self.policy.maximum_files || bytes > self.policy.maximum_bytes {
            return Err(ReplayArchiveError::InvalidRetentionPolicy);
        }
        sync_directory(&self.root)
    }
}

fn create_private_directory(path: &Path) -> Result<(), ReplayArchiveError> {
    fs::create_dir_all(path).map_err(|source| io_error("create directory", source))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|source| io_error("restrict directory permissions", source))?;
    }
    Ok(())
}

fn restrict_private_file(path: &Path) -> Result<(), ReplayArchiveError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|source| io_error("restrict file permissions", source))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn replay_filename(match_id: MatchId, result_id: AuthorityResultId) -> String {
    format!(
        "match-{}-result-{}.{}",
        hex(match_id.as_bytes()),
        hex(result_id.as_bytes()),
        REPLAY_FILE_EXTENSION
    )
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[usize::from(byte >> 4)] as char);
        output.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    output
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, ReplayArchiveError> {
    let file = File::open(path).map_err(|source| io_error("open file", source))?;
    let metadata = file
        .metadata()
        .map_err(|source| io_error("read metadata", source))?;
    if metadata.len() > MAX_REPLAY_BYTES as u64 {
        return Err(ReplayArchiveError::FileTooLarge {
            bytes: metadata.len(),
            maximum: MAX_REPLAY_BYTES,
        });
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_REPLAY_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error("read file", source))?;
    if bytes.len() > MAX_REPLAY_BYTES {
        return Err(ReplayArchiveError::FileTooLarge {
            bytes: bytes.len() as u64,
            maximum: MAX_REPLAY_BYTES,
        });
    }
    Ok(bytes)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), ReplayArchiveError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error("sync directory", source))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), ReplayArchiveError> {
    // Windows does not expose a portable std API for opening and flushing a
    // directory handle. The file itself is still fully flushed before publish.
    Ok(())
}

fn io_error(operation: &'static str, source: io::Error) -> ReplayArchiveError {
    ReplayArchiveError::Io { operation, source }
}

struct TemporaryCleanup {
    path: Option<PathBuf>,
}

impl TemporaryCleanup {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for TemporaryCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::authority::AuthoritySimulation;
    use crate::headless::build_headless_simulation;
    use crate::match_config::{MatchBuildOptions, build_headless_match_config};
    use crate::network_protocol::{
        AuthorityKind, InputFrame, InputSequence, PeerId, SeatOwner, StateHash,
    };
    use crate::replay::{
        AcceptedFighterInput, AuthorityMatchResult, FinalAuthorityResult, ReplayHashCheckpoint,
        ReplayHeader, ReplayInputSource, ReplayTickInputs,
    };
    use crate::{game_state::LocalSetup, simulation::SimTick};

    fn fixture() -> Replay {
        let setup = LocalSetup::default();
        let match_id = MatchId::new(*b"archive-fixture1").unwrap();
        let peer = PeerId::new(7).unwrap();
        let options = MatchBuildOptions::single_peer(
            match_id,
            AuthorityKind::Listen,
            false,
            peer,
            &setup,
            SimTick(120),
        );
        let config = build_headless_match_config(&setup, options).unwrap();
        let manifest = config.manifest;
        let simulation = build_headless_simulation(config).unwrap();
        let initial_snapshot = simulation.capture_snapshot().unwrap();
        let initial_hash = StateHash(initial_snapshot.canonical_hash().unwrap());
        let final_tick = initial_snapshot.header.tick.next();
        let mut inputs = ReplayTickInputs::all_inactive(final_tick);
        for assignment in manifest.ownership.as_slice() {
            inputs.fighters[assignment.fighter.index()] = AcceptedFighterInput {
                fighter: assignment.fighter,
                source: match assignment.owner {
                    SeatOwner::Peer(_) => ReplayInputSource::Peer,
                    SeatOwner::AuthorityBot => ReplayInputSource::AuthorityBot,
                },
                frame: InputFrame {
                    tick: final_tick,
                    seat: assignment.seat,
                    sequence: InputSequence(1),
                    ..InputFrame::default()
                },
            };
        }
        let final_hash = StateHash(0xaced_0000_0000_0001);
        let result_id = AuthorityResultId::new([0x5a; 16]).unwrap();
        Replay {
            header: ReplayHeader::new(
                manifest.compatibility,
                manifest.match_id,
                manifest.manifest_hash,
                initial_snapshot.header.gameplay_content_hash,
                manifest.master_gameplay_seed,
            ),
            initial_snapshot,
            inputs: vec![inputs],
            hash_checkpoints: vec![
                ReplayHashCheckpoint {
                    tick: SimTick::ZERO,
                    state_hash: initial_hash,
                },
                ReplayHashCheckpoint {
                    tick: final_tick,
                    state_hash: final_hash,
                },
            ],
            keyframes: Vec::new(),
            final_result: FinalAuthorityResult {
                result_id,
                confirmed_tick: final_tick,
                state_hash: final_hash,
                result: AuthorityMatchResult::Draw,
            },
        }
    }

    fn temporary_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "afc-replay-archive-{label}-{}-{}",
            std::process::id(),
            NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn save_is_atomic_loadable_and_idempotent() {
        let root = temporary_root("round-trip");
        let archive = ReplayArchive::new(&root);
        let replay = fixture();

        let first = archive.save(&replay).unwrap();
        assert_eq!(first.disposition, ReplaySaveDisposition::Created);
        assert!(first.encoded_bytes <= MAX_REPLAY_BYTES);
        let second = archive.save(&replay).unwrap();
        assert_eq!(second.disposition, ReplaySaveDisposition::AlreadyPresent);
        assert_eq!(
            archive
                .load(replay.header.match_id, replay.final_result.result_id)
                .unwrap(),
            replay
        );
        assert!(
            !fs::read_dir(&root)
                .unwrap()
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&first.path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(&root).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn conflicting_result_bytes_and_oversized_files_fail_closed() {
        let root = temporary_root("fail-closed");
        let archive = ReplayArchive::new(&root);
        let replay = fixture();
        let stored = archive.save(&replay).unwrap();

        let mut conflicting = replay.clone();
        conflicting.final_result.state_hash = StateHash(99);
        conflicting.hash_checkpoints.last_mut().unwrap().state_hash = StateHash(99);
        assert!(matches!(
            archive.save(&conflicting),
            Err(ReplayArchiveError::ConflictingExistingReplay(_))
        ));

        OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&stored.path)
            .unwrap()
            .set_len(MAX_REPLAY_BYTES as u64 + 1)
            .unwrap();
        assert!(matches!(
            archive.load(replay.header.match_id, replay.final_result.result_id),
            Err(ReplayArchiveError::FileTooLarge { .. })
        ));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn filename_uses_only_fixed_width_lowercase_protocol_ids() {
        let match_id = MatchId::new([0xab; 16]).unwrap();
        let result_id = AuthorityResultId::new([0xcd; 16]).unwrap();
        assert_eq!(
            replay_filename(match_id, result_id),
            "match-abababababababababababababababab-result-cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd.afcr"
        );
    }

    #[test]
    fn retention_keeps_exact_file_and_byte_bounds_without_removing_new_save() {
        let root = temporary_root("retention");
        let archive = ReplayArchive::with_policy(
            &root,
            ReplayRetentionPolicy {
                maximum_files: 2,
                maximum_bytes: MAX_REPLAY_BYTES as u64 * 2,
            },
        )
        .unwrap();
        let mut latest_path = None;
        for byte in 1..=4 {
            let mut replay = fixture();
            replay.final_result.result_id = AuthorityResultId::new([byte; 16]).unwrap();
            latest_path = Some(archive.save(&replay).unwrap().path);
        }
        let retained: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry.path().extension().and_then(|value| value.to_str())
                    == Some(REPLAY_FILE_EXTENSION)
            })
            .map(|entry| entry.path())
            .collect();
        assert_eq!(retained.len(), 2);
        assert!(latest_path.unwrap().exists());
        assert!(
            retained
                .iter()
                .map(|path| fs::metadata(path).unwrap().len())
                .sum::<u64>()
                <= archive.policy().maximum_bytes
        );

        fs::remove_dir_all(root).unwrap();
    }
}
