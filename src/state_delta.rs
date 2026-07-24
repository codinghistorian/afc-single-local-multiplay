//! Bounded byte patches for authoritative canonical snapshots.
//!
//! Deltas are deliberately independent of the snapshot schema: both endpoints
//! first produce the canonical snapshot encoding, then replace only changed byte
//! runs. A delta always names its base and target snapshot at the protocol layer;
//! this module owns only the hostile-input-safe patch body.

use crate::network_protocol::MAX_RESYNC_SNAPSHOT_BYTES;

/// Leaves enough room in a 1,200-byte datagram for AFC runtime framing, state
/// identity, processed-input acknowledgements, and the fixed packet header.
pub const MAX_STATE_DELTA_BYTES: usize = 960;
const PATCH_RUN_HEADER_BYTES: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapshotByteDelta {
    target_len: u32,
    payload_len: u16,
    run_count: u16,
    payload: [u8; MAX_STATE_DELTA_BYTES],
}

impl Default for SnapshotByteDelta {
    fn default() -> Self {
        Self {
            target_len: 0,
            payload_len: 0,
            run_count: 0,
            payload: [0; MAX_STATE_DELTA_BYTES],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeltaBuildError {
    SnapshotTooLarge {
        bytes: usize,
        maximum: usize,
    },
    PatchTooLarge {
        required_at_least: usize,
        maximum: usize,
    },
    TooManyRuns,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeltaApplyError {
    BaseTooLarge { bytes: usize, maximum: usize },
    TargetTooLarge { bytes: usize, maximum: usize },
    OutputTooSmall { needed: usize, available: usize },
    InvalidPayloadLength { length: usize },
    TruncatedRunHeader,
    EmptyRun,
    NonCanonicalRunOrder,
    RunOutsideTarget,
    TruncatedRunData,
    RunCountMismatch { declared: u16, decoded: u16 },
    NonZeroPadding,
}

impl SnapshotByteDelta {
    pub const fn target_len(&self) -> usize {
        self.target_len as usize
    }

    pub const fn payload_len(&self) -> usize {
        self.payload_len as usize
    }

    pub const fn run_count(&self) -> u16 {
        self.run_count
    }

    pub fn payload(&self) -> &[u8] {
        &self.payload[..self.payload_len()]
    }

    pub fn from_canonical_bytes(base: &[u8], target: &[u8]) -> Result<Self, DeltaBuildError> {
        validate_build_size(base.len())?;
        validate_build_size(target.len())?;

        let mut delta = Self {
            target_len: target.len() as u32,
            ..Self::default()
        };
        let mut cursor = 0;
        while cursor < target.len() {
            if base.get(cursor) == Some(&target[cursor]) {
                cursor += 1;
                continue;
            }

            let run_start = cursor;
            cursor += 1;
            while cursor < target.len()
                && base.get(cursor) != Some(&target[cursor])
                && cursor - run_start < u16::MAX as usize
            {
                cursor += 1;
            }
            let run_len = cursor - run_start;
            let required = delta.payload_len() + PATCH_RUN_HEADER_BYTES + run_len;
            if required > MAX_STATE_DELTA_BYTES {
                return Err(DeltaBuildError::PatchTooLarge {
                    required_at_least: required,
                    maximum: MAX_STATE_DELTA_BYTES,
                });
            }
            let run_count = delta
                .run_count
                .checked_add(1)
                .ok_or(DeltaBuildError::TooManyRuns)?;
            let write = delta.payload_len();
            delta.payload[write..write + 2].copy_from_slice(&(run_start as u16).to_be_bytes());
            delta.payload[write + 2..write + 4].copy_from_slice(&(run_len as u16).to_be_bytes());
            delta.payload[write + 4..required].copy_from_slice(&target[run_start..cursor]);
            delta.payload_len = required as u16;
            delta.run_count = run_count;
        }
        Ok(delta)
    }

    /// Reconstructs the target encoding into caller-owned bounded storage and
    /// returns its exact logical length.
    pub fn apply(&self, base: &[u8], output: &mut [u8]) -> Result<usize, DeltaApplyError> {
        if base.len() > MAX_RESYNC_SNAPSHOT_BYTES {
            return Err(DeltaApplyError::BaseTooLarge {
                bytes: base.len(),
                maximum: MAX_RESYNC_SNAPSHOT_BYTES,
            });
        }
        let target_len = self.target_len();
        if target_len > MAX_RESYNC_SNAPSHOT_BYTES {
            return Err(DeltaApplyError::TargetTooLarge {
                bytes: target_len,
                maximum: MAX_RESYNC_SNAPSHOT_BYTES,
            });
        }
        if output.len() < target_len {
            return Err(DeltaApplyError::OutputTooSmall {
                needed: target_len,
                available: output.len(),
            });
        }
        self.validate()?;

        let copied = base.len().min(target_len);
        output[..copied].copy_from_slice(&base[..copied]);
        output[copied..target_len].fill(0);

        let payload = self.payload();
        let mut cursor = 0;
        while cursor < payload.len() {
            let offset = u16::from_be_bytes([payload[cursor], payload[cursor + 1]]) as usize;
            let run_len = u16::from_be_bytes([payload[cursor + 2], payload[cursor + 3]]) as usize;
            cursor += PATCH_RUN_HEADER_BYTES;
            output[offset..offset + run_len].copy_from_slice(&payload[cursor..cursor + run_len]);
            cursor += run_len;
        }
        Ok(target_len)
    }

    pub fn validate(&self) -> Result<(), DeltaApplyError> {
        let payload_len = self.payload_len();
        if payload_len > MAX_STATE_DELTA_BYTES {
            return Err(DeltaApplyError::InvalidPayloadLength {
                length: payload_len,
            });
        }
        let target_len = self.target_len();
        if target_len > MAX_RESYNC_SNAPSHOT_BYTES {
            return Err(DeltaApplyError::TargetTooLarge {
                bytes: target_len,
                maximum: MAX_RESYNC_SNAPSHOT_BYTES,
            });
        }
        if self.payload[payload_len..].iter().any(|byte| *byte != 0) {
            return Err(DeltaApplyError::NonZeroPadding);
        }

        let payload = self.payload();
        let mut cursor = 0;
        let mut decoded_runs = 0_u16;
        let mut previous_end = 0_usize;
        while cursor < payload.len() {
            if payload.len() - cursor < PATCH_RUN_HEADER_BYTES {
                return Err(DeltaApplyError::TruncatedRunHeader);
            }
            let offset = u16::from_be_bytes([payload[cursor], payload[cursor + 1]]) as usize;
            let run_len = u16::from_be_bytes([payload[cursor + 2], payload[cursor + 3]]) as usize;
            cursor += PATCH_RUN_HEADER_BYTES;
            if run_len == 0 {
                return Err(DeltaApplyError::EmptyRun);
            }
            if decoded_runs != 0 && offset < previous_end {
                return Err(DeltaApplyError::NonCanonicalRunOrder);
            }
            let end = offset
                .checked_add(run_len)
                .ok_or(DeltaApplyError::RunOutsideTarget)?;
            if end > target_len {
                return Err(DeltaApplyError::RunOutsideTarget);
            }
            if payload.len() - cursor < run_len {
                return Err(DeltaApplyError::TruncatedRunData);
            }
            cursor += run_len;
            previous_end = end;
            decoded_runs = decoded_runs.saturating_add(1);
        }
        if decoded_runs != self.run_count {
            return Err(DeltaApplyError::RunCountMismatch {
                declared: self.run_count,
                decoded: decoded_runs,
            });
        }
        Ok(())
    }

    /// Wire decoder constructor. Padding is validated before the value can be
    /// applied to a baseline.
    pub fn from_wire_parts(
        target_len: u32,
        run_count: u16,
        payload: &[u8],
    ) -> Result<Self, DeltaApplyError> {
        if payload.len() > MAX_STATE_DELTA_BYTES {
            return Err(DeltaApplyError::InvalidPayloadLength {
                length: payload.len(),
            });
        }
        let mut delta = Self {
            target_len,
            payload_len: payload.len() as u16,
            run_count,
            payload: [0; MAX_STATE_DELTA_BYTES],
        };
        delta.payload[..payload.len()].copy_from_slice(payload);
        delta.validate()?;
        Ok(delta)
    }
}

fn validate_build_size(bytes: usize) -> Result<(), DeltaBuildError> {
    if bytes > MAX_RESYNC_SNAPSHOT_BYTES {
        Err(DeltaBuildError::SnapshotTooLarge {
            bytes,
            maximum: MAX_RESYNC_SNAPSHOT_BYTES,
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(base: &[u8], delta: &SnapshotByteDelta) -> Vec<u8> {
        let mut output = vec![0; MAX_RESYNC_SNAPSHOT_BYTES];
        let len = delta.apply(base, &mut output).unwrap();
        output.truncate(len);
        output
    }

    #[test]
    fn sparse_changes_round_trip_without_copying_unchanged_ranges() {
        let base = vec![7; 2_000];
        let mut target = base.clone();
        target[10..13].copy_from_slice(&[1, 2, 3]);
        target[1_050] = 9;
        target[1_999] = 4;
        let delta = SnapshotByteDelta::from_canonical_bytes(&base, &target).unwrap();
        assert_eq!(delta.run_count(), 3);
        assert!(delta.payload_len() < 32);
        assert_eq!(apply(&base, &delta), target);
    }

    #[test]
    fn target_growth_and_truncation_are_exact() {
        let base = b"canonical-base";
        let grown = b"canonical-base-plus";
        let grow_delta = SnapshotByteDelta::from_canonical_bytes(base, grown).unwrap();
        assert_eq!(apply(base, &grow_delta), grown);

        let shortened = b"canonical";
        let truncate_delta = SnapshotByteDelta::from_canonical_bytes(grown, shortened).unwrap();
        assert_eq!(truncate_delta.run_count(), 0);
        assert_eq!(apply(grown, &truncate_delta), shortened);
    }

    #[test]
    fn dense_change_fails_closed_when_it_cannot_fit_one_datagram() {
        let base = vec![0; MAX_STATE_DELTA_BYTES + 1];
        let target = vec![1; MAX_STATE_DELTA_BYTES + 1];
        assert!(matches!(
            SnapshotByteDelta::from_canonical_bytes(&base, &target),
            Err(DeltaBuildError::PatchTooLarge { .. })
        ));
    }

    #[test]
    fn wire_constructor_rejects_overlap_truncation_and_out_of_range_runs() {
        let overlapping = [0, 2, 0, 2, 7, 8, 0, 3, 0, 1, 9];
        assert_eq!(
            SnapshotByteDelta::from_wire_parts(8, 2, &overlapping),
            Err(DeltaApplyError::NonCanonicalRunOrder)
        );

        let truncated = [0, 2, 0, 3, 7, 8];
        assert_eq!(
            SnapshotByteDelta::from_wire_parts(8, 1, &truncated),
            Err(DeltaApplyError::TruncatedRunData)
        );

        let outside = [0, 7, 0, 2, 7, 8];
        assert_eq!(
            SnapshotByteDelta::from_wire_parts(8, 1, &outside),
            Err(DeltaApplyError::RunOutsideTarget)
        );
    }

    #[test]
    fn identical_snapshots_encode_as_empty_patch() {
        let bytes = b"same";
        let delta = SnapshotByteDelta::from_canonical_bytes(bytes, bytes).unwrap();
        assert_eq!(delta.payload_len(), 0);
        assert_eq!(delta.run_count(), 0);
        assert_eq!(apply(bytes, &delta), bytes);
    }
}
