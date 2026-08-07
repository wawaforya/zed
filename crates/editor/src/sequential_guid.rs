use rand::RngCore;
use time::OffsetDateTime;
use uuid::Uuid;

const DOTNET_UNIX_EPOCH_OFFSET_MILLISECONDS: i128 = 62_135_596_800_000;

pub(crate) fn create() -> Uuid {
    let mut random_bytes = [0; 10];
    rand::rng().fill_bytes(&mut random_bytes);
    from_parts(current_timestamp_milliseconds(), random_bytes)
}

pub(crate) fn current_timestamp_milliseconds() -> u64 {
    let unix_milliseconds = OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000;
    (unix_milliseconds + DOTNET_UNIX_EPOCH_OFFSET_MILLISECONDS) as u64
}

fn from_parts(timestamp_milliseconds: u64, random_bytes: [u8; 10]) -> Uuid {
    let timestamp_bytes = timestamp_milliseconds.to_be_bytes();
    let mut guid_bytes = [0; 16];
    guid_bytes[..6].copy_from_slice(&timestamp_bytes[2..]);
    guid_bytes[6..].copy_from_slice(&random_bytes);
    Uuid::from_bytes(guid_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequential_guid_matches_dotnet_byte_order() {
        let guid = from_parts(
            0x0000_1122_3344_5566,
            [0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09],
        );

        assert_eq!(guid.to_string(), "11223344-5566-0001-0203-040506070809");
    }

    #[test]
    fn sequential_guid_uses_low_48_timestamp_bits() {
        let guid = from_parts(0xaabb_1122_3344_5566, [0; 10]);

        assert_eq!(guid.to_string(), "11223344-5566-0000-0000-000000000000");
    }
}
