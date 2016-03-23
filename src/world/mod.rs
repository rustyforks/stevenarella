// Copyright 2015 Matthew Collins
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

pub mod block;

use std::sync::Arc;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use types::bit;
use types::nibble;
use protocol;

pub struct World {
    chunks: HashMap<CPos, Chunk>,
}

impl World {
    pub fn new() -> World {
        World {
            chunks: HashMap::new(),
        }
    }

    pub fn is_chunk_loaded(&self, x: i32, z: i32) -> bool {
        self.chunks.contains_key(&CPos(x, z))
    }

    pub fn set_block(&mut self, x: i32, y: i32, z: i32, b: block::Block) {
        let cpos = CPos(x >> 4, z >> 4);
        if !self.chunks.contains_key(&cpos) {
            self.chunks.insert(cpos, Chunk::new(cpos));
        }
        let chunk = self.chunks.get_mut(&cpos).unwrap();
        chunk.set_block(x & 0xF, y, z & 0xF, b);
    }

    pub fn get_block(&self, x: i32, y: i32, z: i32) -> block::Block {
        match self.chunks.get(&CPos(x >> 4, z >> 4)) {
            Some(ref chunk) => chunk.get_block(x & 0xF, y, z & 0xF),
            None => block::Missing{},
        }
    }

    pub fn get_dirty_chunk_sections(&mut self) -> Vec<(i32, i32, i32, Arc<SectionKey>)> {
        let mut out = vec![];
        for (_, chunk) in &mut self.chunks {
            for sec in &mut chunk.sections {
                if let Some(sec) = sec.as_mut() {
                    if !sec.building && sec.dirty {
                        out.push((chunk.position.0, sec.y as i32, chunk.position.1, sec.key.clone()));
                    }
                }
            }
        }
        out
    }

    pub fn set_building_flag(&mut self, pos: (i32, i32, i32)) {
        if let Some(chunk) = self.chunks.get_mut(&CPos(pos.0, pos.2)) {
            if let Some(sec) = chunk.sections[pos.1 as usize].as_mut() {
                sec.building = true;
                sec.dirty = false;
            }
        }
    }

    pub fn reset_building_flag(&mut self, pos: (i32, i32, i32)) {
        if let Some(chunk) = self.chunks.get_mut(&CPos(pos.0, pos.2)) {
            if let Some(section) = chunk.sections[pos.1 as usize].as_mut() {
                section.building = false;
            }
        }
    }

    pub fn flag_dirty_all(&mut self) {
        for (_, chunk) in &mut self.chunks {
            for sec in &mut chunk.sections {
                if let Some(sec) = sec.as_mut() {
                    sec.dirty = true;
                }
            }
        }
    }

    pub fn capture_snapshot(&self, x: i32, y: i32, z: i32, w: i32, h: i32, d: i32) -> Snapshot {
        use std::cmp::{min, max};
        let mut snapshot = Snapshot {
            blocks: vec![0; (w * h * d) as usize],
            block_light: nibble::Array::new((w * h * d) as usize),
            sky_light: nibble::Array::new((w * h * d) as usize),
            biomes: vec![0; (w * d) as usize],

            x: x, y: y, z: z,
            w: w, h: h, d: d,
        };
        for i in 0 .. (w * h * d) as usize {
            snapshot.sky_light.set(i, 0xF);
            snapshot.blocks[i] = block::Missing{}.get_steven_id() as u16;
        }

        let cx1 = x >> 4;
        let cy1 = y >> 4;
        let cz1 = z >> 4;
        let cx2 = (x + w + 15) >> 4;
        let cy2 = (y + h + 15) >> 4;
        let cz2 = (z + d + 15) >> 4;

        for cx in cx1 .. cx2 {
            for cz in cz1 .. cz2 {
                let chunk = match self.chunks.get(&CPos(cx, cz)) {
                    Some(val) => val,
                    None => continue,
                };

                let x1 = min(16, max(0, x - (cx<<4)));
                let x2 = min(16, max(0, x + w - (cx<<4)));
                let z1 = min(16, max(0, z - (cz<<4)));
                let z2 = min(16, max(0, z + d - (cz<<4)));

                for cy in cy1 .. cy2 {
                    if cy < 0 || cy > 15 {
                        continue;
                    }
                    let section = &chunk.sections[cy as usize];
                    let y1 = min(16, max(0, y - (cy<<4)));
                    let y2 = min(16, max(0, y + h - (cy<<4)));

                    for yy in y1 .. y2 {
                        for zz in z1 .. z2 {
                            for xx in x1 .. x2 {
                                let ox = xx + (cx << 4);
                                let oy = yy + (cy << 4);
                                let oz = zz + (cz << 4);
                                match section.as_ref() {
                                    Some(sec) => {
                                        snapshot.set_block(ox, oy, oz, sec.get_block(xx, yy, zz));
                                        snapshot.set_block_light(ox, oy, oz, sec.get_block_light(xx, yy, zz));
                                        snapshot.set_sky_light(ox, oy, oz, sec.get_sky_light(xx, yy, zz));
                                    },
                                    None => {
                                        snapshot.set_block(ox, oy, oz, block::Air{});
                                    },
                                }
                            }
                        }
                    }
                }
                // TODO: Biomes
            }
        }

        snapshot
    }

    pub fn unload_chunk(&mut self, x: i32, z: i32) {
        self.chunks.remove(&CPos(x, z));
    }

    pub fn load_chunk(&mut self, x: i32, z: i32, new: bool, mask: u16, data: Vec<u8>) -> Result<(), protocol::Error> {
        use std::io::{Cursor, Read};
        use byteorder::ReadBytesExt;
        use protocol::{VarInt, Serializable, LenPrefixed};

        let mut data = Cursor::new(data);

        let cpos = CPos(x, z);
        {
            let chunk = if new {
                self.chunks.insert(cpos, Chunk::new(cpos));
                self.chunks.get_mut(&cpos).unwrap()
            } else {
                if !self.chunks.contains_key(&cpos) {
                    return Ok(());
                }
                self.chunks.get_mut(&cpos).unwrap()
            };

            for i in 0 .. 16 {
                if mask & (1 << i) == 0 {
                    continue;
                }
                if chunk.sections[i].is_none() {
                    chunk.sections[i] = Some(Section::new(x, i as u8, z));
                }
                let section = chunk.sections[i as usize].as_mut().unwrap();
                section.dirty = true;

                let bit_size = try!(data.read_u8());
                let mut block_map = HashMap::with_hasher(BuildHasherDefault::<FNVHash>::default());
                if bit_size <= 8 {
                    let count = try!(VarInt::read_from(&mut data)).0;
                    for i in 0 .. count {
                        let id = try!(VarInt::read_from(&mut data)).0;
                        block_map.insert(i as usize, id);
                    }
                }

                let bits = try!(LenPrefixed::<VarInt, u64>::read_from(&mut data)).data;
                let m = bit::Map::from_raw(bits, bit_size as usize);

                for i in 0 .. 4096 {
                    let val = m.get(i);
                    let block_id = block_map.get(&val).map(|v| *v as usize).unwrap_or(val);
                    let block = block::Block::by_vanilla_id(block_id);
                    let i = i as i32;
                    section.set_block(
                        i & 0xF,
                        i >> 8,
                        (i >> 4) & 0xF,
                        block
                    );
                }

                try!(data.read_exact(&mut section.block_light.data));
                try!(data.read_exact(&mut section.sky_light.data));
            }
        }
        for i in 0 .. 16 {
            if mask & (1 << i) == 0 {
                continue;
            }
            for pos in [
                (-1, 0, 0), (1, 0, 0),
                (0, -1, 0), (0, 1, 0),
                (0, 0, -1), (0, 0, 1)].into_iter() {
                self.flag_section_dirty(x + pos.0, i as i32 + pos.1, z + pos.2);
            }
        }
        Ok(())
    }

    fn flag_section_dirty(&mut self, x: i32, y: i32, z: i32) {
        if y < 0 || y > 15 {
            return;
        }
        let cpos = CPos(x, z);
        if let Some(chunk) = self.chunks.get_mut(&cpos) {
            if let Some(sec) = chunk.sections[y as usize].as_mut() {
                sec.dirty = true;
            }
        }
    }
}

pub struct Snapshot {
    blocks: Vec<u16>,
    block_light: nibble::Array,
    sky_light: nibble::Array,
    biomes: Vec<u8>,

    x: i32,
    y: i32,
    z: i32,
    w: i32,
    h: i32,
    d: i32,
}

impl Snapshot {

    pub fn make_relative(&mut self, x: i32, y: i32, z: i32) {
        self.x = x;
        self.y = y;
        self.z = z;
    }

    pub fn get_block(&self, x: i32, y: i32, z: i32) -> block::Block {
        block::Block::by_steven_id(self.blocks[self.index(x, y, z)] as usize)
    }

    pub fn set_block(&mut self, x: i32, y: i32, z: i32, b: block::Block) {
        let idx = self.index(x, y, z);
        self.blocks[idx] = b.get_steven_id() as u16;
    }

    pub fn get_block_light(&self, x: i32, y: i32, z: i32) -> u8 {
        self.block_light.get(self.index(x, y, z))
    }

    pub fn set_block_light(&mut self, x: i32, y: i32, z: i32, l: u8) {
        let idx = self.index(x, y, z);
        self.block_light.set(idx, l);
    }

    pub fn get_sky_light(&self, x: i32, y: i32, z: i32) -> u8 {
        self.sky_light.get(self.index(x, y, z))
    }

    pub fn set_sky_light(&mut self, x: i32, y: i32, z: i32, l: u8) {
        let idx = self.index(x, y, z);
        self.sky_light.set(idx, l);
    }

    #[inline]
    fn index(&self, x: i32, y: i32, z: i32) -> usize {
        ((x - self.x) + ((z - self.z) * self.w) + ((y - self.y) * self.w * self.d)) as usize
    }
}

#[derive(PartialEq, Eq, Hash, Clone, Copy)]
pub struct CPos(i32, i32);

pub struct Chunk {
    position: CPos,

    sections: [Option<Section>; 16],
    biomes: [u8; 16 * 16],
}

impl Chunk {
    fn new(pos: CPos) -> Chunk {
        Chunk {
            position: pos,
            sections: [
                None,None,None,None,
                None,None,None,None,
                None,None,None,None,
                None,None,None,None,
            ],
            biomes: [0; 16 * 16],
        }
    }

    fn set_block(&mut self, x: i32, y: i32, z: i32, b: block::Block) {
        let s_idx = y >> 4;
        if s_idx < 0 || s_idx > 15 {
            return;
        }
        if self.sections[s_idx as usize].is_none() {
            if let block::Air {} = b {
                return;
            }
            self.sections[s_idx as usize] = Some(Section::new(self.position.0, s_idx as u8, self.position.1));
        }
        let section = self.sections[s_idx as usize].as_mut().unwrap();
        section.set_block(x, y & 0xF, z, b);
    }

    fn get_block(&self, x: i32, y: i32, z: i32) -> block::Block {
        let s_idx = y >> 4;
        if s_idx < 0 || s_idx > 15 {
            return block::Missing{};
        }
        match self.sections[s_idx as usize].as_ref() {
            Some(sec) => sec.get_block(x, y & 0xF, z),
            None => block::Air{},
        }
    }
}

struct FNVHash(u64);

impl Hasher for FNVHash {
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = self.0.wrapping_mul(0x100000001b3);
            self.0 ^= *b as u64
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

impl Default for FNVHash {
    fn default() -> Self {
        FNVHash(0xcbf29ce484222325)
    }
}

#[derive(PartialEq, Eq, Hash)]
pub struct SectionKey {
    pos: (i32, u8, i32),
}

struct Section {
    key: Arc<SectionKey>,
    y: u8,

    blocks: bit::Map,
    block_map: Vec<(block::Block, u32)>,
    rev_block_map: HashMap<block::Block, usize, BuildHasherDefault<FNVHash>>,

    block_light: nibble::Array,
    sky_light: nibble::Array,

    dirty: bool,
    building: bool,
}

impl Section {
    fn new(x: i32, y: u8, z: i32) -> Section {
        let mut section = Section {
            key: Arc::new(SectionKey{
                pos: (x, y, z),
            }),
            y: y,

            blocks: bit::Map::new(4096, 4),
            block_map: vec![
                (block::Air{}, 0xFFFFFFFF)
            ],
            rev_block_map: HashMap::with_hasher(BuildHasherDefault::default()),

            block_light: nibble::Array::new(16 * 16 * 16),
            sky_light: nibble::Array::new(16 * 16 * 16),

            dirty: false,
            building: false,
        };
        for i in 0 .. 16*16*16 {
            section.sky_light.set(i, 0xF);
        }
        section.rev_block_map.insert(block::Air{}, 0);
        section
    }

    fn get_block(&self, x: i32, y: i32, z: i32) -> block::Block {
        let idx = self.blocks.get(((y << 8) | (z << 4) | x) as usize);
        self.block_map[idx].0
    }

    fn set_block(&mut self, x: i32, y: i32, z: i32, b: block::Block) {
        let old = self.get_block(x, y, z);
        if old == b {
            return;
        }
        // Clean up old block
        {
            let idx = self.rev_block_map[&old];
            let info = &mut self.block_map[idx];
            info.1 -= 1;
            if info.1 == 0 { // None left of this type
                self.rev_block_map.remove(&old);
            }
        }

        if !self.rev_block_map.contains_key(&b) {
            let mut found = false;
            for (i, ref mut info) in self.block_map.iter_mut().enumerate() {
                if info.1 == 0 {
                    info.0 = b;
                    self.rev_block_map.insert(b, i);
                    found = true;
                    break;
                }
            }
            if !found {
                if self.block_map.len() >= 1 << self.blocks.bit_size {
                    let new_size = self.blocks.bit_size << 1;
                    let new_blocks = self.blocks.resize(new_size);
                    self.blocks = new_blocks;
                }
                self.rev_block_map.insert(b, self.block_map.len());
                self.block_map.push((b, 0));
            }
        }

        let idx = self.rev_block_map[&b];
        let info = &mut self.block_map[idx];
        info.1 += 1;
        self.blocks.set(((y << 8) | (z << 4) | x) as usize, idx);
        self.dirty = true;
    }

    fn get_block_light(&self, x: i32, y: i32, z: i32) -> u8 {
        self.block_light.get(((y << 8) | (z << 4) | x) as usize)
    }

    fn set_block_light(&mut self, x: i32, y: i32, z: i32, l: u8) {
        self.block_light.set(((y << 8) | (z << 4) | x) as usize, l);
    }

    fn get_sky_light(&self, x: i32, y: i32, z: i32) -> u8 {
        self.sky_light.get(((y << 8) | (z << 4) | x) as usize)
    }

    fn set_sky_light(&mut self, x: i32, y: i32, z: i32, l: u8) {
        self.sky_light.set(((y << 8) | (z << 4) | x) as usize, l);
    }
}