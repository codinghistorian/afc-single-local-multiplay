//! Transport admission framing shared by browser and hosted-server builds.
//!
//! This small pre-protocol exchange is deliberately separate from AFC
//! datagrams. A transport is not handed to the gameplay authority until the
//! server has verified and consumed the embedded one-time join ticket.

use core::fmt;

const ADMISSION_MAGIC: [u8; 4] = *b"AFCA";
const ADMISSION_VERSION: u8 = 1;
const ADMISSION_HEADER_BYTES: usize = 7;

pub const MAX_ADMISSION_TICKET_BYTES: usize = 384;
pub const MAX_ADMISSION_FRAME_BYTES: usize = ADMISSION_HEADER_BYTES + MAX_ADMISSION_TICKET_BYTES;
pub const ADMISSION_ACCEPTED_FRAME: [u8; 5] = *b"AFCO\x01";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WebAdmissionFrameError;

impl fmt::Display for WebAdmissionFrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("malformed AFC web admission frame")
    }
}

impl std::error::Error for WebAdmissionFrameError {}

pub fn encode_admission_request(ticket: &str) -> Result<Vec<u8>, WebAdmissionFrameError> {
    if ticket.is_empty() || ticket.len() > MAX_ADMISSION_TICKET_BYTES {
        return Err(WebAdmissionFrameError);
    }
    let ticket_len = u16::try_from(ticket.len()).map_err(|_| WebAdmissionFrameError)?;
    let mut frame = Vec::with_capacity(ADMISSION_HEADER_BYTES + ticket.len());
    frame.extend_from_slice(&ADMISSION_MAGIC);
    frame.push(ADMISSION_VERSION);
    frame.extend_from_slice(&ticket_len.to_be_bytes());
    frame.extend_from_slice(ticket.as_bytes());
    Ok(frame)
}

pub fn decode_admission_request(frame: &[u8]) -> Result<&str, WebAdmissionFrameError> {
    if frame.len() < ADMISSION_HEADER_BYTES
        || frame.get(..4) != Some(ADMISSION_MAGIC.as_slice())
        || frame.get(4).copied() != Some(ADMISSION_VERSION)
    {
        return Err(WebAdmissionFrameError);
    }
    let ticket_len = usize::from(u16::from_be_bytes(
        frame[5..7].try_into().map_err(|_| WebAdmissionFrameError)?,
    ));
    if ticket_len == 0
        || ticket_len > MAX_ADMISSION_TICKET_BYTES
        || frame.len() != ADMISSION_HEADER_BYTES + ticket_len
    {
        return Err(WebAdmissionFrameError);
    }
    std::str::from_utf8(&frame[ADMISSION_HEADER_BYTES..]).map_err(|_| WebAdmissionFrameError)
}

pub fn is_admission_accepted(frame: &[u8]) -> bool {
    frame == ADMISSION_ACCEPTED_FRAME
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_request_is_exact_bounded_and_versioned() {
        let ticket = "ticket-body";
        let frame = encode_admission_request(ticket).unwrap();
        assert_eq!(decode_admission_request(&frame), Ok(ticket));
        assert!(frame.len() <= MAX_ADMISSION_FRAME_BYTES);

        let mut trailing = frame;
        trailing.push(0);
        assert_eq!(
            decode_admission_request(&trailing),
            Err(WebAdmissionFrameError)
        );
        assert_eq!(
            encode_admission_request(&"x".repeat(MAX_ADMISSION_TICKET_BYTES + 1)),
            Err(WebAdmissionFrameError)
        );
        assert!(is_admission_accepted(&ADMISSION_ACCEPTED_FRAME));
        assert!(!is_admission_accepted(b"AFCO\x02"));
    }
}
