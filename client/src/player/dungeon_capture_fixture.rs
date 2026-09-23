//! The first dungeon's capture fixture: the production instance, exported voxel for voxel
//! by `server/internal/world/dungeon_capture_test.go` (`VHDUNG01`). Test-only; it has no
//! runtime player wiring and decides nothing.
//!
//! The export is whole chunks — the dungeon's shell and a halo of void around it, all of it
//! loaded — so every chunk it spans is inserted, air included, and a production ray never
//! mistakes loaded void for terrain that has not arrived.
use crate::net::ChunkCoord;
use crate::world::{BlockId, ChunkStore, VoxelChunk};
use bevy::prelude::*;
use std::collections::BTreeMap;

const HEADER: usize = 96;
const CHUNK: i64 = 32;
/// The unrotated drawing's extent (`dungeonWidth`, `dungeonHeight`, `dungeonDepth`).
pub(crate) const DRAWING: [i64; 3] = [35, 60, 106];

#[derive(Debug)]
pub(crate) struct DungeonFixture {
    pub worldgen: u32,
    pub seed: i64,
    pub facing: u32,
    pub opened: bool,
    /// World cell of the placed drawing's corner.
    pub building_origin: [i64; 3],
    /// World cell of the exported volume's corner, chunk aligned.
    pub origin: [i64; 3],
    pub size: [usize; 3],
    pub blocks: Vec<BlockId>,
}

impl DungeonFixture {
    /// Checks everything before allocating the payload: header, dimensions, chunk alignment,
    /// that the placed drawing lies inside the volume, the exact length and every block id.
    pub(crate) fn parse(data: &[u8], known_blocks: &[BlockId]) -> Result<Self, &'static str> {
        if data.len() < HEADER || &data[..8] != b"VHDUNG01" {
            return Err("missing dungeon fixture header");
        }
        let u32_at = |i: usize| u32::from_le_bytes(data[i..i + 4].try_into().unwrap());
        let i64_at = |i: usize| i64::from_le_bytes(data[i..i + 8].try_into().unwrap());
        if u32_at(8) != 1 || u32_at(24) > 3 || u32_at(28) > 1 || u32_at(92) != 0 {
            return Err("unsupported fixture configuration");
        }
        let size = [
            u32_at(80) as usize,
            u32_at(84) as usize,
            u32_at(88) as usize,
        ];
        if size
            .iter()
            .any(|&n| n == 0 || n > 256 || n % CHUNK as usize != 0)
        {
            return Err("invalid dimensions");
        }
        let volume = size.iter().product::<usize>();
        if volume > 16_000_000 || data.len() != HEADER + volume * 2 {
            return Err("invalid payload length");
        }
        let origin = [i64_at(56), i64_at(64), i64_at(72)];
        let building_origin = [i64_at(32), i64_at(40), i64_at(48)];
        let facing = u32_at(24);
        let footprint = if facing % 2 == 0 {
            DRAWING
        } else {
            [DRAWING[2], DRAWING[1], DRAWING[0]]
        };
        for axis in 0..3 {
            if origin[axis].rem_euclid(CHUNK) != 0 {
                return Err("volume is not chunk aligned");
            }
            let end = origin[axis]
                .checked_add(size[axis] as i64)
                .ok_or("origin overflow")?;
            if i32::try_from(origin[axis].div_euclid(CHUNK)).is_err()
                || i32::try_from((end - 1).div_euclid(CHUNK)).is_err()
            {
                return Err("origin exceeds chunk coordinate range");
            }
            let drawing_end = building_origin[axis]
                .checked_add(footprint[axis])
                .ok_or("drawing origin overflow")?;
            if building_origin[axis] < origin[axis] || drawing_end > end {
                return Err("dungeon lies outside exported volume");
            }
        }
        let mut blocks = Vec::with_capacity(volume);
        for v in data[HEADER..].chunks_exact(2) {
            let block = u16::from_le_bytes(v.try_into().unwrap());
            if !known_blocks.contains(&block) {
                return Err("fixture contains an unknown block");
            }
            blocks.push(block);
        }
        Ok(Self {
            worldgen: u32_at(12),
            seed: i64_at(16),
            facing,
            opened: u32_at(28) == 1,
            building_origin,
            origin,
            size,
            blocks,
        })
    }

    /// The block in one world cell; air outside the export, which is the instance's void.
    pub(crate) fn world_block(&self, cell: [i64; 3]) -> BlockId {
        let mut index = 0usize;
        for axis in [1, 2, 0] {
            let local = cell[axis] - self.origin[axis];
            if local < 0 || local >= self.size[axis] as i64 {
                return crate::world::palette::AIR;
            }
            index = index * self.size[axis] + local as usize;
        }
        self.blocks[index]
    }

    /// The block in one cell of the unrotated drawing. Only an unturned dungeon is read
    /// this way: the review cameras and the arena crop are written in the drawing's frame.
    pub(crate) fn drawing_block(&self, x: i64, y: i64, z: i64) -> BlockId {
        assert_eq!(
            self.facing, 0,
            "drawing-frame reads need an unturned dungeon"
        );
        let o = self.building_origin;
        self.world_block([o[0] + x, o[1] + y, o[2] + z])
    }

    /// A continuous point of the unrotated drawing, in world space.
    pub(crate) fn drawing_point(&self, point: [f32; 3]) -> Vec3 {
        assert_eq!(
            self.facing, 0,
            "drawing-frame points need an unturned dungeon"
        );
        let o = self.building_origin;
        Vec3::new(
            o[0] as f32 + point[0],
            o[1] as f32 + point[1],
            o[2] as f32 + point[2],
        )
    }

    /// Inserts every exported chunk, all-air ones included, and answers how many.
    pub(crate) fn place_chunks(&self, store: &mut ChunkStore) -> usize {
        let mut chunks = BTreeMap::new();
        let [width, height, depth] = self.size;
        for y in 0..height {
            for z in 0..depth {
                for x in 0..width {
                    let world = [
                        self.origin[0] + x as i64,
                        self.origin[1] + y as i64,
                        self.origin[2] + z as i64,
                    ];
                    let coord = (
                        world[0].div_euclid(CHUNK) as i32,
                        world[1].div_euclid(CHUNK) as i32,
                        world[2].div_euclid(CHUNK) as i32,
                    );
                    let chunk = chunks
                        .entry(coord)
                        .or_insert_with(|| VoxelChunk::all_air(CHUNK as usize));
                    let block = self.blocks[(y * depth + z) * width + x];
                    if block != crate::world::palette::AIR {
                        chunk.set(
                            world[0].rem_euclid(CHUNK) as usize,
                            world[1].rem_euclid(CHUNK) as usize,
                            world[2].rem_euclid(CHUNK) as usize,
                            block,
                        );
                    }
                }
            }
        }
        let count = chunks.len();
        for ((cx, cy, cz), chunk) in chunks {
            store.insert(ChunkCoord { cx, cy, cz }, chunk);
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic() -> Vec<u8> {
        let mut bytes = Vec::from(*b"VHDUNG01");
        for n in [1u32, 37] {
            bytes.extend(n.to_le_bytes());
        }
        bytes.extend(0i64.to_le_bytes());
        for n in [0u32, 1] {
            bytes.extend(n.to_le_bytes());
        }
        for n in [-17i64, -50, -53, -64, -96, -96] {
            bytes.extend(n.to_le_bytes());
        }
        for n in [128u32, 160, 192, 0] {
            bytes.extend(n.to_le_bytes());
        }
        bytes.resize(HEADER + 128 * 160 * 192 * 2, 0);
        bytes
    }

    #[test]
    fn dungeon_fixture_reads_the_export_frame() {
        let mut data = synthetic();
        // One block at the drawing's corner, which is world (-17, -50, -53).
        let index = ((-50i64 + 96) * 192 + (-53 + 96)) * 128 + (-17 + 64);
        data[HEADER + index as usize * 2] = 7;
        let fixture = DungeonFixture::parse(&data, &[0, 7]).unwrap();
        assert!(fixture.opened);
        assert_eq!(fixture.drawing_block(0, 0, 0), 7);
        assert_eq!(fixture.drawing_block(1, 0, 0), 0);
        assert_eq!(fixture.world_block([10_000, 0, 0]), 0);
        assert_eq!(
            fixture.drawing_point([0.5, 1.0, 0.5]),
            Vec3::new(-16.5, -49.0, -52.5)
        );
        let mut store = ChunkStore::default();
        assert_eq!(fixture.place_chunks(&mut store), 4 * 5 * 6);
    }

    #[test]
    fn dungeon_fixture_rejects_malformed_before_allocating() {
        let original = synthetic();
        for len in [0, 7, 95, original.len() - 1] {
            assert!(DungeonFixture::parse(&original[..len], &[0]).is_err());
        }
        for (offset, n) in [(8, 2u32), (24, 4), (28, 2), (80, 257), (84, 0), (88, 100)] {
            let mut bad = original.clone();
            bad[offset..offset + 4].copy_from_slice(&n.to_le_bytes());
            assert!(DungeonFixture::parse(&bad, &[0]).is_err());
        }
        let mut bad = original.clone();
        bad.push(0);
        assert!(DungeonFixture::parse(&bad, &[0]).is_err());
        bad = original.clone();
        bad[HEADER..HEADER + 2].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(DungeonFixture::parse(&bad, &[0]).is_err());
        // The drawing must lie inside the volume, and the volume on chunk boundaries.
        bad = original.clone();
        bad[32..40].copy_from_slice(&60i64.to_le_bytes());
        assert!(DungeonFixture::parse(&bad, &[0]).is_err());
        bad = original;
        bad[56..64].copy_from_slice(&(-63i64).to_le_bytes());
        assert!(DungeonFixture::parse(&bad, &[0]).is_err());
    }
}
