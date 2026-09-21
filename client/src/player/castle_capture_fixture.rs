#[derive(Debug)]
pub struct CastleFixture {
    pub worldgen: u32,
    pub seed: i64,
    pub actual_facing: u32,
    pub review_turn: u32,
    pub scene_mode: u32,
    pub building_origin: [i64; 3],
    pub origin: [i64; 3],
    pub size: [usize; 3],
    pub blocks: Vec<u16>,
}
impl CastleFixture {
    pub fn parse(data: &[u8], known_blocks: &[u16]) -> Result<Self, &'static str> {
        if data.len() < 96 || &data[..8] != b"VHCAST03" {
            return Err("missing castle fixture header");
        }
        let u32_at = |i| u32::from_le_bytes(data[i..i + 4].try_into().unwrap());
        let i64_at = |i| i64::from_le_bytes(data[i..i + 8].try_into().unwrap());
        if u32_at(8) != 3
            || u32_at(24) > 3
            || u32_at(28) > 3
            || u32_at(32) > 1
            || (u32_at(32) == 1 && u32_at(28) != 0)
        {
            return Err("unsupported fixture configuration");
        }
        let size = [
            u32_at(84) as usize,
            u32_at(88) as usize,
            u32_at(92) as usize,
        ];
        if size.iter().any(|&n| n == 0 || n > 256) {
            return Err("invalid dimensions");
        }
        let volume = size
            .iter()
            .try_fold(1usize, |n, &d| n.checked_mul(d))
            .ok_or("volume overflow")?;
        if volume > 16_000_000 || data.len() != 96 + volume * 2 {
            return Err("invalid payload length");
        }
        let origin = [i64_at(60), i64_at(68), i64_at(76)];
        let building_origin = [i64_at(36), i64_at(44), i64_at(52)];
        for axis in 0..3 {
            let end = origin[axis]
                .checked_add(size[axis] as i64)
                .ok_or("origin overflow")?;
            if i32::try_from(origin[axis].div_euclid(32)).is_err()
                || i32::try_from((end - 1).div_euclid(32)).is_err()
            {
                return Err("origin exceeds chunk coordinate range");
            }
            let building_end = building_origin[axis]
                .checked_add(if axis == 1 { 68 } else { 63 })
                .ok_or("building origin overflow")?;
            if building_origin[axis] < origin[axis] || building_end > end {
                return Err("castle lies outside exported volume");
            }
        }
        if data[96..]
            .chunks_exact(2)
            .any(|v| !known_blocks.contains(&u16::from_le_bytes(v.try_into().unwrap())))
        {
            return Err("fixture contains an unknown block");
        }
        let blocks = data[96..]
            .chunks_exact(2)
            .map(|v| u16::from_le_bytes(v.try_into().unwrap()))
            .collect();
        Ok(Self {
            worldgen: u32_at(12),
            seed: i64_at(16),
            actual_facing: u32_at(24),
            review_turn: u32_at(28),
            scene_mode: u32_at(32),
            building_origin,
            origin,
            size,
            blocks,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn synthetic() -> Vec<u8> {
        let mut bytes = Vec::from(*b"VHCAST03");
        for n in [3u32, 1] {
            bytes.extend(n.to_le_bytes());
        }
        bytes.extend(0x5eedi64.to_le_bytes());
        for n in [0u32, 0, 0] {
            bytes.extend(n.to_le_bytes());
        }
        for n in [85i64, 64, 80, 85, 63, 80] {
            bytes.extend(n.to_le_bytes());
        }
        for n in [63u32, 69, 63] {
            bytes.extend(n.to_le_bytes());
        }
        bytes.resize(96 + 63 * 69 * 63 * 2, 0);
        bytes
    }
    #[test]
    fn validates_fixture_frames() {
        let fixture = CastleFixture::parse(&synthetic(), &[0]).unwrap();
        assert_eq!(fixture.building_origin, [85, 64, 80]);
        assert_eq!(fixture.size, [63, 69, 63]);
    }
    #[test]
    fn rejects_malformed_before_allocating() {
        let original = synthetic();
        for len in [0, 7, 95, original.len() - 1] {
            assert!(CastleFixture::parse(&original[..len], &[0]).is_err());
        }
        for (offset, n) in [(8, 4u32), (24, 4), (28, 4), (32, 2), (84, 257), (88, 0)] {
            let mut bad = original.clone();
            bad[offset..offset + 4].copy_from_slice(&n.to_le_bytes());
            assert!(CastleFixture::parse(&bad, &[0]).is_err());
        }
        let mut bad = original.clone();
        bad.push(0);
        assert!(CastleFixture::parse(&bad, &[0]).is_err());
        bad = original.clone();
        bad[96..98].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(CastleFixture::parse(&bad, &[0]).is_err());
        bad = original;
        bad[60..68].copy_from_slice(&i64::MAX.to_le_bytes());
        assert!(CastleFixture::parse(&bad, &[0]).is_err());
    }
    #[test]
    #[ignore = "requires actual opt-in Go fixture export"]
    fn reads_actual_go_export() {
        let data = std::fs::read(std::env::var("CASTLE_CAPTURE_FIXTURE").unwrap()).unwrap();
        let fixture = CastleFixture::parse(&data, &(0..=59).collect::<Vec<_>>()).unwrap();
        assert_eq!(fixture.seed, 0x5eed);
        assert_eq!(fixture.actual_facing, 0);
    }
}
