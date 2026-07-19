//! Bounded, length-delimited blocking framing.

use std::io::{self, Read, Write};

use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;

pub const LENGTH_PREFIX_SIZE: usize = 2;
pub const MAX_FRAME_SIZE: usize = u16::MAX as usize;
pub const MAX_MESSAGE_SIZE: usize = 60 * 1024;

pub fn write_frame<W>(writer: &mut W, frame: &[u8]) -> Result<(), FramingError>
where
    W: Write,
{
    if frame.is_empty() {
        return Err(FramingError::EmptyFrame);
    }
    if frame.len() > MAX_FRAME_SIZE {
        return Err(FramingError::FrameTooLarge {
            actual: frame.len(),
            maximum: MAX_FRAME_SIZE,
        });
    }
    let length = u16::try_from(frame.len()).map_err(|_| FramingError::FrameTooLarge {
        actual: frame.len(),
        maximum: MAX_FRAME_SIZE,
    })?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(frame)?;
    writer.flush()?;
    Ok(())
}

pub fn read_frame<R>(reader: &mut R) -> Result<Vec<u8>, FramingError>
where
    R: Read,
{
    let mut prefix = [0_u8; LENGTH_PREFIX_SIZE];
    reader.read_exact(&mut prefix)?;
    let length = usize::from(u16::from_be_bytes(prefix));
    if length == 0 {
        return Err(FramingError::EmptyFrame);
    }
    let mut frame = vec![0_u8; length];
    reader.read_exact(&mut frame)?;
    Ok(frame)
}

pub fn write_message<W, T>(writer: &mut W, message: &T) -> Result<(), FramingError>
where
    W: Write,
    T: Serialize,
{
    let encoded = encode_message(message)?;
    write_frame(writer, &encoded)
}

pub fn read_message<R, T>(reader: &mut R) -> Result<T, FramingError>
where
    R: Read,
    T: DeserializeOwned,
{
    let frame = read_frame(reader)?;
    decode_message(&frame)
}

pub(crate) fn encode_message<T>(message: &T) -> Result<Vec<u8>, FramingError>
where
    T: Serialize,
{
    let encoded = postcard::to_stdvec(message).map_err(FramingError::Serialize)?;
    if encoded.len() > MAX_MESSAGE_SIZE {
        return Err(FramingError::MessageTooLarge {
            actual: encoded.len(),
            maximum: MAX_MESSAGE_SIZE,
        });
    }
    Ok(encoded)
}

/// Decodes one already-delimited application message while enforcing the
/// workspace message-size limit and rejecting trailing bytes.
pub fn decode_message<T>(encoded: &[u8]) -> Result<T, FramingError>
where
    T: DeserializeOwned,
{
    if encoded.len() > MAX_MESSAGE_SIZE {
        return Err(FramingError::MessageTooLarge {
            actual: encoded.len(),
            maximum: MAX_MESSAGE_SIZE,
        });
    }
    let (message, remainder) =
        postcard::take_from_bytes(encoded).map_err(FramingError::Deserialize)?;
    if !remainder.is_empty() {
        return Err(FramingError::TrailingBytes(remainder.len()));
    }
    Ok(message)
}

#[derive(Debug, Error)]
pub enum FramingError {
    #[error("I/O error while reading or writing a frame")]
    Io(#[from] io::Error),
    #[error("zero-length frames are forbidden")]
    EmptyFrame,
    #[error("frame is {actual} bytes; maximum is {maximum}")]
    FrameTooLarge { actual: usize, maximum: usize },
    #[error("serialized message is {actual} bytes; maximum is {maximum}")]
    MessageTooLarge { actual: usize, maximum: usize },
    #[error("could not serialize message")]
    Serialize(#[source] postcard::Error),
    #[error("could not deserialize message")]
    Deserialize(#[source] postcard::Error),
    #[error("message has {0} trailing bytes")]
    TrailingBytes(usize),
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, ErrorKind};

    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Fixture {
        id: u64,
        text: String,
    }

    #[test]
    fn frame_prefix_is_big_endian_and_round_trips() {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, b"abc").expect("write frame");
        assert_eq!(&bytes[..2], &[0, 3]);
        assert_eq!(
            read_frame(&mut Cursor::new(bytes)).expect("read frame"),
            b"abc"
        );
    }

    #[test]
    fn fragmented_reader_is_supported() {
        struct OneByteReader(Cursor<Vec<u8>>);
        impl Read for OneByteReader {
            fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
                let length = output.len().min(1);
                self.0.read(&mut output[..length])
            }
        }

        let mut bytes = Vec::new();
        write_frame(&mut bytes, b"fragmented").expect("write frame");
        let mut reader = OneByteReader(Cursor::new(bytes));
        assert_eq!(read_frame(&mut reader).expect("read frame"), b"fragmented");
    }

    #[test]
    fn empty_oversized_and_truncated_frames_fail_closed() {
        assert!(matches!(
            write_frame(&mut Vec::new(), &[]),
            Err(FramingError::EmptyFrame)
        ));
        assert!(matches!(
            write_frame(&mut Vec::new(), &vec![0; MAX_FRAME_SIZE + 1]),
            Err(FramingError::FrameTooLarge { .. })
        ));
        let error = read_frame(&mut Cursor::new(vec![0, 3, 1])).expect_err("truncated");
        assert!(
            matches!(error, FramingError::Io(error) if error.kind() == ErrorKind::UnexpectedEof)
        );
    }

    #[test]
    fn postcard_message_round_trips_and_rejects_trailing_bytes() {
        let fixture = Fixture {
            id: 42,
            text: "RustView".into(),
        };
        let mut bytes = Vec::new();
        write_message(&mut bytes, &fixture).expect("write message");
        let decoded: Fixture = read_message(&mut Cursor::new(bytes)).expect("read message");
        assert_eq!(decoded, fixture);

        let mut encoded = encode_message(&fixture).expect("encode");
        encoded.push(0);
        assert!(matches!(
            decode_message::<Fixture>(&encoded),
            Err(FramingError::TrailingBytes(1))
        ));
    }

    #[test]
    fn message_limit_is_enforced_before_framing() {
        let oversized = vec![0_u8; MAX_MESSAGE_SIZE + 1];
        assert!(matches!(
            write_message(&mut Vec::new(), &oversized),
            Err(FramingError::MessageTooLarge { .. })
        ));
    }
}
