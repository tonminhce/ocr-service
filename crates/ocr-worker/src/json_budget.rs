use std::io::{self, Write};

use serde::Serialize;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum JsonBudgetError {
    Limit,
    Serialization,
}

struct ByteCounter {
    limit: usize,
    bytes: usize,
    exceeded: bool,
}

impl Write for ByteCounter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() > self.limit.saturating_sub(self.bytes) {
            self.exceeded = true;
            return Err(io::Error::other("serialized JSON exceeds its byte budget"));
        }
        self.bytes += buffer.len();
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn serialized_bytes<Serialized: Serialize + ?Sized>(
    value: &Serialized,
    limit: usize,
) -> Result<usize, JsonBudgetError> {
    let mut counter = ByteCounter {
        limit,
        bytes: 0,
        exceeded: false,
    };
    let result = serde_json::to_writer(&mut counter, value);
    if counter.exceeded {
        return Err(JsonBudgetError::Limit);
    }
    result.map_err(|_| JsonBudgetError::Serialization)?;
    Ok(counter.bytes)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use serde::{Serializer, ser::SerializeSeq};
    use serde_json::json;

    use super::*;

    #[test]
    fn compact_bytes_match_json_with_unicode_escapes_and_nested_values() {
        for value in [
            json!(null),
            json!({}),
            json!([]),
            json!("Tiếng Việt 🦀\n\t\r\u{0000}\"\\/"),
            json!({"nested": [true, false, null, -32, 1.25e20, {"địa chỉ": "Hà Nội"}]}),
        ] {
            let expected = serde_json::to_vec(&value).unwrap().len();
            assert_eq!(serialized_bytes(&value, expected), Ok(expected));
            assert_eq!(serialized_bytes(&value, usize::MAX), Ok(expected));
            assert_eq!(
                serialized_bytes(&value, expected - 1),
                Err(JsonBudgetError::Limit)
            );
            assert_eq!(serialized_bytes(&value, 0), Err(JsonBudgetError::Limit));
        }
        assert_eq!(serialized_bytes("đ", 4), Ok(4));
    }

    struct SequenceWithTail<'a>(&'a Cell<bool>);

    impl Serialize for SequenceWithTail<'_> {
        fn serialize<SerializerType: Serializer>(
            &self,
            serializer: SerializerType,
        ) -> Result<SerializerType::Ok, SerializerType::Error> {
            let mut sequence = serializer.serialize_seq(Some(2))?;
            sequence.serialize_element("synthetic oversized element")?;
            self.0.set(true);
            sequence.serialize_element("unvisited tail")?;
            sequence.end()
        }
    }

    #[test]
    fn limit_failure_stops_before_serializing_the_remaining_elements() {
        let tail_visited = Cell::new(false);
        assert_eq!(
            serialized_bytes(&SequenceWithTail(&tail_visited), 4),
            Err(JsonBudgetError::Limit)
        );
        assert!(!tail_visited.get());
    }

    struct InvalidSerialization;

    impl Serialize for InvalidSerialization {
        fn serialize<SerializerType: Serializer>(
            &self,
            _serializer: SerializerType,
        ) -> Result<SerializerType::Ok, SerializerType::Error> {
            Err(serde::ser::Error::custom("synthetic serializer failure"))
        }
    }

    #[test]
    fn serializer_failure_is_distinct_from_budget_exhaustion() {
        assert_eq!(
            serialized_bytes(&InvalidSerialization, 1024),
            Err(JsonBudgetError::Serialization)
        );
    }

    #[test]
    fn overflowing_write_consumes_no_bytes_and_never_wraps_the_counter() {
        let mut counter = ByteCounter {
            limit: usize::MAX,
            bytes: usize::MAX - 1,
            exceeded: false,
        };
        assert!(counter.write(&[0, 1]).is_err());
        assert_eq!(counter.bytes, usize::MAX - 1);
        assert!(counter.exceeded);
    }
}
