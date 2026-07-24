//! Frozen deterministic constants for canonical simulation math.
//!
//! C1 reference provenance:
//! - source worktree base: d33ceff65065e18d0928820892bb24bfb5c845ae
//! - rustc: 1.94.1 (e408947bf 2026-03-25), LLVM 21.1.8
//! - target: aarch64-apple-darwin
//! - reference date: 2026-07-23
//! - Bee source SHA-256: 92ee33353abc8245d5d0aadb99659359d03c8aced3acd04c3d6f1b47c3b400
//! - Chick source SHA-256: 14aa16f2eeaaf65497c0f90561f76e35cd160a297e3fcc97fe17be9f12639ca1
//! - Arena source SHA-256: b18cad60d8573c5f50480b76d6a233e1f5148812447b6e1d4ca55d783a1b99dc
//! - Champion's Court RON SHA-256: 015157b7527b52eb536116f1b115002ed18880d132565a7bc55be78980168ff0
//!
//! Values were emitted by temporary in-module tests that invoked the pre-C1
//! private production helpers. Normal builds never regenerate these values
//! through host libm. Presentation-only animation retains platform trigonometry.

use bevy::prelude::{Vec2, Vec3};

/// Version of the scalar-ordered canonical vector math contract.
///
/// This is source-hash input rather than a wire field. The simulation
/// compatibility version remains the externally negotiated boundary.
#[allow(dead_code)]
pub(crate) const CANONICAL_MATH_REVISION: u16 = 1;

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CollisionYawBasisBits {
    pub(crate) yaw: u32,
    pub(crate) cos: u32,
    pub(crate) sin: u32,
}

#[cfg(test)]
impl CollisionYawBasisBits {
    pub(crate) const fn new(yaw: u32, cos: u32, sin: u32) -> Self {
        Self { yaw, cos, sin }
    }
}

pub(crate) const BEE_HONEY_PUDDLE_SCALE_BITS: [u32; 145] = [
    0x3f733333, 0x3f7748ac, 0x3f7b4b96, 0x3f7f29b6, 0x3f8168bc, 0x3f831921, 0x3f849e5c, 0x3f85f184,
    0x3f870c95, 0x3f87ea87, 0x3f88876a, 0x3f88e076, 0x3f88f415, 0x3f88c1ef, 0x3f884ae7, 0x3f87911a,
    0x3f8697d5, 0x3f856384, 0x3f83f9a1, 0x3f826098, 0x3f809fab, 0x3f7d7da7, 0x3f798d34, 0x3f757fe4,
    0x3f74fe46, 0x3f790de3, 0x3f7d02e6, 0x3f8065a9, 0x3f822afc, 0x3f83c95f, 0x3f853978, 0x3f8674bd,
    0x3f877596, 0x3f883774, 0x3f88b6e5, 0x3f88f1a6, 0x3f88e6ae, 0x3f88962c, 0x3f880190, 0x3f872b7c,
    0x3f8617be, 0x3f84cb3a, 0x3f834bd7, 0x3f81a065, 0x3f7fa0f0, 0x3f7bc89d, 0x3f77c947, 0x3f73b519,
    0x3f76c7c8, 0x3f7acdfe, 0x3f7eb1a5, 0x3f813088, 0x3f82e5c1, 0x3f8470b9, 0x3f85ca6e, 0x3f86ecbc,
    0x3f87d27e, 0x3f88779d, 0x3f88d92d, 0x3f88f571, 0x3f88cbea, 0x3f885d54, 0x3f87aba5, 0x3f86ba05,
    0x3f858cbe, 0x3f842929, 0x3f829596, 0x3f80d930, 0x3f7df7b1, 0x3f7a0c14, 0x3f76015a, 0x3f747c87,
    0x3f788e2a, 0x3f7c8775, 0x3f802b2e, 0x3f81f4c7, 0x3f839867, 0x3f850e9b, 0x3f8650bf, 0x3f87591a,
    0x3f8822fb, 0x3f88aacc, 0x3f88ee25, 0x3f88ebd4, 0x3f88a3e2, 0x3f881797, 0x3f874971, 0x3f863d18,
    0x3f84f74f, 0x3f837de0, 0x3f81d77e, 0x3f800ba7, 0x3f7c450b, 0x3f784991, 0x3f7436f7, 0x3f7646a4,
    0x3f7a4fde, 0x3f7e38c7, 0x3f80f7cc, 0x3f82b1ba, 0x3f844254, 0x3f85a27d, 0x3f86cbf6, 0x3f87b976,
    0x3f8866c7, 0x3f88d0d4, 0x3f88f5bb, 0x3f88d4d6, 0x3f886eb9, 0x3f87c534, 0x3f86db4a, 0x3f85b522,
    0x3f8457f5, 0x3f82c9f5, 0x3f811233, 0x3f7e70fa, 0x3f7a8a7a, 0x3f76829b, 0x3f73fab3, 0x3f780e10,
    0x3f7c0b5d, 0x3f7fe079, 0x3f81bdfc, 0x3f8366bc, 0x3f84e2f2, 0x3f862bdd, 0x3f873ba7, 0x3f880d7e,
    0x3f889da6, 0x3f88e992, 0x3f88efe8, 0x3f88b08b, 0x3f882c9c, 0x3f876671, 0x3f866190, 0x3f85229a,
    0x3f83af38, 0x3f820e03, 0x3f804662, 0x3f7cc0d7, 0x3f78c980, 0x3f74b8c5, 0x3f75c546, 0x3f79d141,
    0x3f7dbf25,
];

#[cfg(test)]
pub(crate) const BEE_HONEY_PUDDLE_SCALE_BITS_FNV1A64: u64 = 0xabb9749f53e3fbd4;

pub(crate) const BEE_ULTIMATE_SWARM_SCALE_BITS: [u32; 145] = [
    0x3f75c28f, 0x3f79d428, 0x3f7dbc3b, 0x3f80a975, 0x3f8239c9, 0x3f837f25, 0x3f846c90, 0x3f84f894,
    0x3f851d9a, 0x3f84da2a, 0x3f8430f3, 0x3f8328b4, 0x3f81cbf7, 0x3f8028a3, 0x3f7c9edd, 0x3f78a66f,
    0x3f76f49c, 0x3f7afe55, 0x3f7ed2a4, 0x3f81253a, 0x3f82a131, 0x3f83ce10, 0x3f849fd9, 0x3f850e2e,
    0x3f8514aa, 0x3f84b30b, 0x3f83ed34, 0x3f82cb09, 0x3f81581b, 0x3f7f466a, 0x3f7b7b80, 0x3f777630,
    0x3f78259d, 0x3f7c23ef, 0x3f7fe123, 0x3f819b88, 0x3f8301d5, 0x3f841530, 0x3f84ca9f, 0x3f851ae7,
    0x3f8502d3, 0x3f848359, 0x3f83a18f, 0x3f826675, 0x3f80de9b, 0x3f7e333f, 0x3f7a5325, 0x3f764473,
    0x3f795489, 0x3f7d43f7, 0x3f807366, 0x3f820bf6, 0x3f835b60, 0x3f845448, 0x3f84ecbf, 0x3f851eb2,
    0x3f84e823, 0x3f844b3e, 0x3f834e47, 0x3f81fb50, 0x3f805fe0, 0x3f7d18b6, 0x3f7926ce, 0x3f7672d6,
    0x3f7a8058, 0x3f7e5d72, 0x3f80f15e, 0x3f827621, 0x3f83ad85, 0x3f848b1f, 0x3f850619, 0x3f85198c,
    0x3f84c4b1, 0x3f840aec, 0x3f82f3a2, 0x3f8189f8, 0x3f7fb8b0, 0x3f7bf7c6, 0x3f77f780, 0x3f77a46c,
    0x3f7ba800, 0x3f7f6f6a, 0x3f816a09, 0x3f82d9af, 0x3f83f7fb, 0x3f84b986, 0x3f851698, 0x3f850b7a,
    0x3f84989e, 0x3f83c299, 0x3f8291f2, 0x3f8112d0, 0x3f7ea8f0, 0x3f7ad16b, 0x3f76c643, 0x3f78d459,
    0x3f7cca85, 0x3f803c77, 0x3f81dd02, 0x3f833646, 0x3f843a83, 0x3f84df55, 0x3f851e2d, 0x3f84f488,
    0x3f846410, 0x3f837286, 0x3f82298b, 0x3f809640, 0x3f7d916b, 0x3f79a6a1, 0x3f75f0f7, 0x3f7a019a,
    0x3f7de6e5, 0x3f80bc8c, 0x3f8249e0, 0x3f838b98, 0x3f8474e0, 0x3f84fc6c, 0x3f851cd2, 0x3f84d4ca,
    0x3f842733, 0x3f831af7, 0x3f81bac9, 0x3f8014b4, 0x3f7c7312, 0x3f787877, 0x3f7722eb, 0x3f7b2b22,
    0x3f7efc2d, 0x3f813785, 0x3f82b048, 0x3f83d95a, 0x3f84a6e2, 0x3f8510ae, 0x3f851288, 0x3f84ac5c,
    0x3f83e23d, 0x3f82bc3b, 0x3f81460d, 0x3f7f1d3a, 0x3f7b4ee3, 0x3f7747ee, 0x3f7853ab, 0x3f7c4ffa,
    0x3f8004b1,
];

#[cfg(test)]
pub(crate) const BEE_ULTIMATE_SWARM_SCALE_BITS_FNV1A64: u64 = 0x2d177831a3b51e22;

pub(crate) const CHICK_SUNNY_SPLASH_SCALE_BITS: [u32; 70] = [
    0x3f47ae14, 0x3f4abd90, 0x3f4dbb74, 0x3f50968d, 0x3f533e6e, 0x3f55a3d4, 0x3f57b8f8, 0x3f5971e0,
    0x3f5ac4a6, 0x3f5ba9ae, 0x3f5c1bd3, 0x3f5c1886, 0x3f5b9fd8, 0x3f5ab480, 0x3f595bc8, 0x3f579d6c,
    0x3f558373, 0x3f5319f2, 0x3f506ec8, 0x3f4d914b, 0x3f4a91f4, 0x3f47da28, 0x3f4ae91f, 0x3f4de582,
    0x3f50be29, 0x3f5362b5, 0x3f55c3f4, 0x3f57d438, 0x3f5987a5, 0x3f5ad472, 0x3f5bb327, 0x3f5c1ec2,
    0x3f5c14da, 0x3f5b95a6, 0x3f5aa404, 0x3f59455e, 0x3f578197, 0x3f5562d2, 0x3f52f542, 0x3f5046da,
    0x3f4d6705, 0x3f4a664a, 0x3f48063b, 0x3f4b149e, 0x3f4e0f74, 0x3f50e59b, 0x3f5386c4, 0x3f55e3d3,
    0x3f57ef2f, 0x3f599d18, 0x3f5ae3e6, 0x3f5bbc43, 0x3f5c2152, 0x3f5c10cf, 0x3f5b8b18, 0x3f5a932e,
    0x3f592ea4, 0x3f576578, 0x3f5541f2, 0x3f52d05c, 0x3f501ec4, 0x3f4d3ca6, 0x3f4a3a95, 0x3f48324d,
    0x3f4b400e, 0x3f4e3947, 0x3f510ce3, 0x3f53aa9e, 0x3f560370, 0x3f5809da,
];

#[cfg(test)]
pub(crate) const CHICK_SUNNY_SPLASH_SCALE_BITS_FNV1A64: u64 = 0x2f15e72d64f37a7a;

pub(crate) const CHICK_OMELET_FIELD_SCALE_BITS: [u32; 124] = [
    0x3fa3d70a, 0x3fa6e43d, 0x3fa9d24b, 0x3fac834e, 0x3faedbcc, 0x3fb0c3d6, 0x3fb227f7, 0x3fb2f9fc,
    0x3fb33186, 0x3fb2cc5d, 0x3fb1ce8b, 0x3fb0422d, 0x3fae3711, 0x3fabc213, 0x3fa8fc44, 0x3fa601f2,
    0x3fa4bc94, 0x3fa7c3de, 0x3faaa31a, 0x3fad3cf6, 0x3faf76e8, 0x3fb13a37, 0x3fb274e4, 0x3fb31a64,
    0x3fb3241e, 0x3fb291af, 0x3fb168ed, 0x3fafb5ac, 0x3fad8948, 0x3faaf9ee, 0x3fa821bf, 0x3fa51dc3,
    0x3fa5a155, 0x3fa8a012, 0x3fab6df9, 0x3fadee6a, 0x3fb007de, 0x3fb1a4e7, 0x3fb2b50e, 0x3fb32d79,
    0x3fb3095b, 0x3fb24a24, 0x3fb0f776, 0x3faf1ecf, 0x3facd307, 0x3faa2b8e, 0x3fa7437b, 0x3fa43875,
    0x3fa68485, 0x3fa97818, 0x3fac3238, 0x3fae970f, 0x3fb08e2f, 0x3fb2038a, 0x3fb2e83d, 0x3fb33329,
    0x3fb2e153, 0x3fb1f5fd, 0x3fb07a89, 0x3fae7e17, 0x3fac14ee, 0x3fa957a8, 0x3fa66239, 0x3fa45b3f,
    0x3fa76561, 0x3faa4b34, 0x3facef2b, 0x3faf3650, 0x3fb10967, 0x3fb255cd, 0x3fb30e44, 0x3fb32b71,
    0x3fb2ac29, 0x3fb19580, 0x3faff292, 0x3fadd413, 0x3fab4fa3, 0x3fa87ef3, 0x3fa57ebf, 0x3fa54070,
    0x3fa8431f, 0x3fab18ae, 0x3fada42d, 0x3fafcba5, 0x3fb17918, 0x3fb29b68, 0x3fb32702, 0x3fb31655,
    0x3fb26a0c, 0x3fb12905, 0x3faf600a, 0x3fad2156, 0x3faa83d2, 0x3fa7a22f, 0x3fa499d1, 0x3fa62461,
    0x3fa91d03, 0x3fabdfd1, 0x3fae50a1, 0x3fb05688, 0x3fb1dce3, 0x3fb2d41f, 0x3fb33262, 0x3fb2f3eb,
    0x3fb21b36, 0x3fb0b0e7, 0x3faec370, 0x3fac667e, 0x3fa9b22f, 0x3fa6c218, 0x3fa3f9d8, 0x3fa70652,
    0x3fa9f24a, 0x3fac9ff2, 0x3faef3ef, 0x3fb0d683, 0x3fb2346f, 0x3fb2ffc0, 0x3fb3305a, 0x3fb2c44e,
    0x3fb1bfeb, 0x3fb02d91, 0x3fae1d4d, 0x3faba42d,
];

#[cfg(test)]
pub(crate) const CHICK_OMELET_FIELD_SCALE_BITS_FNV1A64: u64 = 0x6698352cb3dbe005;

pub(crate) const CHICK_ORBIT_BASIS_BITS_FLAT: [u32; 962] = [
    0x3f800000, 0x00000000, 0x3f7efc8c, 0x3db60e38, 0x3f7bf43e, 0x3e3555b5, 0x3f76ed3c, 0x3e871a5f,
    0x3f6ff1b6, 0x3eb2780a, 0x3f670fd4, 0x3edc6bf5, 0x3f5c5997, 0x3f02508a, 0x3f4fe4b4, 0x3f1562f5,
    0x3f41ca6d, 0x3f274694, 0x3f322756, 0x3f37d721, 0x3f211b23, 0x3f46f30b, 0x3f0ec861, 0x3f547bb0,
    0x3ef6a869, 0x3f6055a3, 0x3ecdcc17, 0x3f6a68de, 0x3ea34ea0, 0x3f72a0f4, 0x3e6f0c49, 0x3f78ed3c,
    0x3e1596be, 0x3f7d40f4, 0x3d6bc832, 0x3f7f9354, 0xbd00a8c1, 0x3f7fdfa9, 0xbdf60a55, 0x3f7e2558,
    0xbe54e6d8, 0x3f7a67e1, 0xbe968c76, 0x3f74aeda, 0xbec1745e, 0x3f6d05da, 0xbeead41e, 0x3f637c6b,
    0xbf092bf4, 0x3f5825df, 0xbf1bd7ca, 0x3f4b1934, 0xbf2d47c0, 0x3f3c70da, 0xbf3d5877, 0x3f2c4a8c,
    0xbf4be964, 0x3f1ac700, 0xbf58dcfc, 0x3f0809bc, 0xbf641902, 0x3ee8716d, 0xbf6d86ad, 0x3ebef841,
    0xbf7512e6, 0x3e93fbee, 0xbf7aae59, 0x3e4fa769, 0xbf7e4dae, 0x3deb641b, 0xbf7fe98b, 0x3cd6710c,
    0xbf7f7eae, 0xbd809880, 0xbf7d0def, 0xbe1ae42d, 0xbf789c41, 0xbe744224, 0xbf7232a7, 0xbea5d880,
    0xbf69de1b, 0xbed03fd3, 0xbf5faf88, 0xbef900fa, 0xbf53bb8d, 0xbf0fe4b4, 0xbf461a65, 0xbf22253f,
    0xbf36e7ab, 0xbf331d27, 0xbf264238, 0xbf42a9f9, 0xbf144bc4, 0xbf50ac38, 0xbf0128b3, 0xbf5d0781,
    0xbed9ffb5, 0xbf67a2c1, 0xbeaff424, 0xbf70687c, 0xbe8483ec, 0xbf7746eb, 0xbe300e13, 0xbf7c3021,
    0xbdab5f22, 0xbf7f1a28, 0x3bab93e6, 0xbf7fff1a, 0x3dc0bbe3, 0xbf7edd26, 0x3e3a9c0e, 0xbf7bb697,
    0x3e89afe5, 0xbf7691d1, 0x3eb4faad, 0xbf6f7942, 0x3eded69d, 0xbf667b4a, 0x3f037776, 0xbf5baa21,
    0x3f16791c, 0xbf4f1bba, 0x3f2849c0, 0xbf40e986, 0x3f38c548, 0xbf31304b, 0x3f47ca48, 0xbf200fe8,
    0x3f553a5a, 0xbf0dab07, 0x3f60fa2d, 0xbef44e15, 0x3f6af1fb, 0xbecb56e9, 0x3f730d8e, 0xbea0c393,
    0x3f793c78, 0xbe69d4c0, 0x3f7d7231, 0xbe104862, 0x3f7fa62f, 0xbd565e3c, 0x3f7fd3fc, 0x3d161794,
    0x3f7dfb39, 0x3e00578a, 0x3f7a1fa7, 0x3e5a24c9, 0x3f744916, 0x3e991bef, 0x3f6c835c, 0x3ec3ef20,
    0x3f62de3b, 0x3eed352a, 0x3f576d3f, 0x3f0a4d32, 0x3f4a479a, 0x3f1ce77a, 0x3f3b87e5, 0x3f2e43c3,
    0x3f2b4c1c, 0x3f3e3ec5, 0x3f19b51c, 0x3f4cb828, 0x3f06e68d, 0x3f599295, 0x3ee60d1b, 0x3f64b3ff,
    0x3ebc7acd, 0x3f6e05d6, 0x3e916a74, 0x3f757536, 0x3e4a66b4, 0x3f7af30d, 0x3de0bbfb, 0x3f7e743b,
    0x3cab8e15, 0x3f7ff1a1, 0xbd8b4be0, 0x3f7f683c, 0xbe203076, 0x3f7cd924, 0xbe797668, 0x3f784985,
    0xbea8613a, 0x3f71c2a5, 0xbed2b212, 0x3f6951b6, 0xbefb57d6, 0x3f5f07d7, 0xbf110003, 0x3f52f9ed,
    0xbf232e40, 0x3f454054, 0xbf3411ad, 0x3f35f6f4, 0xbf438827, 0x3f253cb2, 0xbf51724a, 0x3f133381,
    0xbf5db3d8, 0x3efffffc, 0xbf68340d, 0x3ed791f3, 0xbf70dd95, 0x3ead6ef4, 0xbf779edd, 0x3e81ec88,
    0xbf7c6a3e, 0x3e2ac546, 0xbf7f35fb, 0x3da0aeab, 0xbf7ffc68, 0xbc2b934c, 0xbf7ebbf5, 0xbdcb6885,
    0xbf7b772d, 0xbe3fe0ea, 0xbf7634ac, 0xbe8c4471, 0xbf6eff1c, 0xbeb77c1a, 0xbf65e521, 0xbee13fbb,
    0xbf5af922, 0xbf049d73, 0xbf4e5148, 0xbf178e39, 0xbf400744, 0xbf294bc0, 0xbf3037f9, 0xbf39b22a,
    0xbf1f038a, 0xbf48a023, 0xbf0c8cbc, 0xbf55f77d, 0xbef1f215, 0xbf619d21, 0xbec8e063, 0xbf6b796d,
    0xbe9e3769, 0xbf737873, 0xbe649b73, 0xbf7989f6, 0xbe0af8f3, 0xbf7da1a7, 0xbd40f205, 0xbf7fb740,
    0x3d2b85da, 0xbf7fc683, 0x3e05a8c3, 0xbf7dcf54, 0x3e5f60e4, 0xbf79d5af, 0x3e9baa3e, 0xbf73e19f,
    0x3ec6689f, 0xbf6bff30, 0x3eef9499, 0xbf623e70, 0x3f0b6d88, 0xbf56b312, 0x3f1df618, 0xbf49748c,
    0x3f2f3e87, 0xbf3a9da5, 0x3f3f23b2, 0xbf2a4c85, 0x3f4d8579, 0xbf18a228, 0x3f5a46a1, 0xbf05c275,
    0x3f654d61, 0xbee3a72a, 0x3f6e8356, 0xbeb9fbf7, 0x3f75d5d0, 0xbe8ed7e5, 0x3f7b3601, 0xbe452454,
    0x3f7e98fd, 0xbdd61306, 0x3f7ff7ea, 0xbc80abe9, 0x3f7f5001, 0x3d95fdc6, 0x3f7ca291, 0x3e257bbe,
    0x3f77f50b, 0x3e7ea8eb, 0x3f7150ef, 0x3eaae8d5, 0x3f68c3aa, 0x3ed522e5, 0x3f5e5e9e, 0x3efdacd3,
    0x3f5236cd, 0x3f121a55, 0x3f4464ec, 0x3f24360f, 0x3f3504eb, 0x3f3504fb, 0x3f2435fe, 0x3f4464fa,
    0x3f121a43, 0x3f5236da, 0x3efdacac, 0x3f5e5ea9, 0x3ed522bc, 0x3f68c3b4, 0x3eaae8ab, 0x3f7150f7,
    0x3e7ea894, 0x3f77f511, 0x3e257b66, 0x3f7ca294, 0x3d95fd13, 0x3f7f5002, 0xbc80aeb8, 0x3f7ff7ea,
    0xbdd613b9, 0x3f7e98fa, 0xbe4524ac, 0x3f7b35fd, 0xbe8ed810, 0x3f75d5c9, 0xbeb9fc21, 0x3f6e834e,
    0xbee3a753, 0x3f654d57, 0xbf05c289, 0x3f5a4696, 0xbf18a23a, 0x3f4d856c, 0xbf2a4c95, 0x3f3f23a3,
    0xbf3a9db4, 0x3f2f3e76, 0xbf49749a, 0x3f1df607, 0xbf56b31e, 0x3f0b6d75, 0xbf623e7b, 0x3eef9471,
    0xbf6bff38, 0x3ec66875, 0xbf73e1a5, 0x3e9baa13, 0xbf79d5b4, 0x3e5f608c, 0xbf7dcf57, 0x3e05a86a,
    0xbf7fc684, 0x3d2b8473, 0xbf7fb73f, 0xbd40f36c, 0xbf7da1a4, 0xbe0af94c, 0xbf7989f1, 0xbe649bcb,
    0xbf73786c, 0xbe9e3793, 0xbf6b7964, 0xbec8e08d, 0xbf619d0e, 0xbef1f259, 0xbf55f779, 0xbf0c8cc2,
    0xbf48a01f, 0xbf1f038f, 0xbf39b21b, 0xbf30380a, 0xbf294baf, 0xbf400753, 0xbf178e34, 0xbf4e514c,
    0xbf049d52, 0xbf5af936, 0xbee13f76, 0xbf65e532, 0xbeb77bf0, 0xbf6eff24, 0xbe8c4445, 0xbf7634b2,
    0xbe3fe092, 0xbf7b7731, 0xbdcb6752, 0xbf7ebbf9, 0xbc2b91ae, 0xbf7ffc68, 0x3da0af5e, 0xbf7f35f9,
    0x3e2ac59f, 0xbf7c6a3a, 0x3e81ecb3, 0xbf779ed7, 0x3ead6f3c, 0xbf70dd88, 0x3ed791ff, 0xbf68340a,
    0x3f000004, 0xbf5db3d5, 0x3f133394, 0xbf51723d, 0x3f253cc3, 0xbf438818, 0x3f35f704, 0xbf34119d,
    0x3f45406d, 0xbf232e22, 0x3f52fa02, 0xbf10ffe4, 0x3f5f07e2, 0xbefb57af, 0x3f6951bf, 0xbed2b1e9,
    0x3f71c2ad, 0xbea86110, 0x3f78498f, 0xbe7975d3, 0x3f7cd92a, 0xbe202fde, 0x3f7f683d, 0xbd8b4b6d,
    0x3f7ff1a0, 0x3cab90e4, 0x3f7e7438, 0x3de0bced, 0x3f7af309, 0x3e4a670c, 0x3f757537, 0x3e916a71,
    0x3f6e05d4, 0x3ebc7ad9, 0x3f64b3f9, 0x3ee60d34, 0x3f599289, 0x3f06e6a0, 0x3f4cb816, 0x3f19b535,
    0x3f3e3eab, 0x3f2b4c39, 0x3f2e43a1, 0x3f3b8805, 0x3f1ce774, 0x3f4a479e, 0x3f0a4d26, 0x3f576d47,
    0x3eed3502, 0x3f62de46, 0x3ec3eee7, 0x3f6c8368, 0x3e991ba5, 0x3f744921, 0x3e5a2491, 0x3f7a1faa,
    0x3e005731, 0x3f7dfb3c, 0x3d1616ad, 0x3f7fd3fd, 0xbd565fa3, 0x3f7fa62e, 0xbe10485c, 0x3f7d7231,
    0xbe69d4d9, 0x3f793c77, 0xbea0c3eb, 0x3f730d80, 0xbecb5712, 0x3f6af1f2, 0xbef44e4b, 0x3f60fa1f,
    0xbf0dab27, 0x3f553a45, 0xbf201000, 0x3f47ca35, 0xbf313050, 0x3f38c543, 0xbf40e990, 0x3f2849b5,
    0xbf4f1bc7, 0x3f16790a, 0xbf5baa31, 0x3f03775c, 0xbf667b57, 0x3eded667, 0xbf6f794f, 0x3eb4fa65,
    0xbf7691d5, 0x3e89afc9, 0xbf7bb6a1, 0x3e3a9b38, 0xbf7edd28, 0x3dc0bb30, 0xbf7fff1a, 0x3bab84aa,
    0xbf7f1a28, 0xbdab5f56, 0xbf7c3019, 0xbe300eca, 0xbf7746e7, 0xbe848408, 0xbf706874, 0xbeaff44e,
    0xbf67a2c2, 0xbed9ffb2, 0xbf5d076e, 0xbf0128d4, 0xbf50ac34, 0xbf144bca, 0xbf42a9f0, 0xbf264243,
    0xbf331d00, 0xbf36e7d1, 0xbf22252e, 0xbf461a73, 0xbf0fe49a, 0xbf53bb9e, 0xbef900ee, 0xbf5faf8b,
    0xbed03f7e, 0xbf69de2e, 0xbea5d865, 0xbf7232ab, 0xbe7441cd, 0xbf789c47, 0xbe1ae433, 0xbf7d0def,
    0xbd80974d, 0xbf7f7eb0, 0x3cd671da, 0xbf7fe98b, 0x3deb648e, 0xbf7e4dac, 0x3e4fa7c1, 0xbf7aae55,
    0x3e93fc28, 0xbf7512dd, 0x3ebef83e, 0xbf6d86ae, 0x3ee87179, 0xbf6418ff, 0x3f0809e0, 0xbf58dce5,
    0x3f1ac70f, 0xbf4be959, 0x3f2c4a9d, 0xbf3d5868, 0x3f3c70ef, 0xbf2d47aa, 0x3f4b1949, 0xbf1bd7af,
    0x3f5825e4, 0xbf092beb, 0x3f637c80, 0xbeead3cc, 0x3f6d05e2, 0xbec17434, 0x3f74aee1, 0xbe968c43,
    0x3f7a67e2, 0xbe54e6cf, 0x3f7e255d, 0xbdf60923, 0x3f7fdfaa, 0xbd00a7d9, 0x3f7f9353, 0x3d6bc959,
    0x3f7d40f5, 0x3e1596a8, 0x3f78ed35, 0x3e6f0cbf, 0x3f72a0e8, 0x3ea34ee5, 0x3f6a68d9, 0x3ecdcc2a,
    0x3f60558c, 0x3ef6a8be, 0x3f547ba4, 0x3f0ec874, 0x3f46f2f9, 0x3f211b39, 0x3f37d71f, 0x3f322758,
    0x3f274675, 0x3f41ca87, 0x3f1562ea, 0x3f4fe4bc, 0x3f02507a, 0x3f5c59a0, 0x3edc6c00, 0x3f670fd1,
    0x3eb277d0, 0x3f6ff1c1, 0x3e871a55, 0x3f76ed3d, 0x3e35558a, 0x3f7bf440, 0x3db60db3, 0x3f7efc8e,
    0xb5b3bc81, 0x3f800000, 0xbdb60f19, 0x3f7efc8a, 0xbe35563b, 0x3f7bf438, 0xbe871aac, 0x3f76ed32,
    0xbeb27824, 0x3f6ff1b1, 0xbedc6c52, 0x3f670fbe, 0xbf0250a0, 0x3f5c598a, 0xbf15630f, 0x3f4fe4a2,
    0xbf274697, 0x3f41ca6a, 0xbf37d73f, 0x3f322738, 0xbf46f315, 0x3f211b16, 0xbf547bbd, 0x3f0ec84e,
    0xbf6055a1, 0x3ef6a86f, 0xbf6a68eb, 0x3ecdcbd8, 0xbf72a0f6, 0x3ea34e90, 0xbf78ed40, 0x3e6f0c11,
    0xbf7d40fb, 0x3e1595f6, 0xbf7f9356, 0x3d6bc68b, 0xbf7fdfa8, 0xbd00aaa8, 0xbf7e2557, 0xbdf60a88,
    0xbf7a67d8, 0xbe54e77f, 0xbf74aed4, 0xbe968c99, 0xbf6d05d1, 0xbec17488, 0xbf637c6c, 0xbeead41c,
    0xbf5825cc, 0xbf092c11, 0xbf4b192e, 0xbf1bd7d3, 0xbf3c70d0, 0xbf2d47cb, 0xbf2c4a7b, 0xbf3d5886,
    0xbf1ac6eb, 0xbf4be974, 0xbf0809ba, 0xbf58dcfd, 0xbee87129, 0xbf641913, 0xbebef7eb, 0xbf6d86bf,
    0xbe93fbd2, 0xbf7512ea, 0xbe4fa711, 0xbf7aae5e, 0xbdeb6329, 0xbf7e4db1, 0xbcd66c3d, 0xbf7fe98c,
    0x3d8098b4, 0xbf7f7ead, 0x3e1ae4e5, 0xbf7d0de8, 0x3e74427c, 0xbf789c3c, 0x3ea5d8ba, 0xbf72329d,
    0x3ed03fd1, 0xbf69de1c, 0x3ef9013d, 0xbf5faf75, 0x3f0fe4bf, 0xbf53bb84, 0x3f222551, 0xbf461a56,
    0x3f331d20, 0xbf36e7b1, 0x3f42aa0d, 0xbf264220, 0x3f50ac4e, 0xbf144ba5, 0x3f5d0785, 0xbf0128ad,
    0x3f67a2d5, 0xbed9ff61, 0x3f706884, 0xbeaff3fa, 0x3f7746f2, 0xbe8483b1, 0x3f7c3021, 0xbe300e19,
    0x3f7f1a2c, 0xbdab5df0, 0x3f7fff1a, 0x3bab9b22, 0x3f7edd24, 0x3dc0bc96, 0x3f7bb698, 0x3e3a9be8,
    0x3f7691c9, 0x3e89b020, 0x3f6f793f, 0x3eb4fab9, 0x3f667b44, 0x3eded6b7, 0x3f5baa19, 0x3f037783,
    0x3f4f1bad, 0x3f16792e, 0x3f40e972, 0x3f2849d7, 0x3f31302f, 0x3f38c562, 0x3f200fc4, 0x3f47ca66,
    0x3f0daae7, 0x3f553a70, 0x3ef44dc4, 0x3f60fa44, 0x3ecb56fa, 0x3f6af1f7, 0x3ea0c31c, 0x3f730da2,
    0x3e69d4a7, 0x3f793c7a, 0x3e104829, 0x3f7d7233, 0x3d565cd5, 0x3f7fa631, 0xbd16197b, 0x3f7fd3fb,
    0xbe0057e3, 0x3f7dfb36, 0xbe5a2540, 0x3f7a1fa0, 0xbe991bbe, 0x3f74491d, 0xbec3ef75, 0x3f6c834b,
    0xbeed358a, 0x3f62de22, 0xbf0a4d31, 0x3f576d40, 0xbf1ce7b1, 0x3f4a476e, 0xbf2e43c2, 0x3f3b87e6,
    0xbf3e3ec9, 0x3f2b4c17, 0xbf4cb830, 0x3f19b511, 0xbf5992a1, 0x3f06e67a, 0xbf64b40d, 0x3ee60ce4,
    0xbf6e05e4, 0x3ebc7a85, 0xbf757531, 0x3e916a95, 0xbf7af318, 0x3e4a65de, 0xbf7e7439, 0x3de0bc87,
    0xbf7ff1a1, 0x3cab8f46, 0xbf7f683c, 0xbd8b4bd4, 0xbf7cd923, 0xbe20308f, 0xbf784984, 0xbe797681,
    0xbf71c29e, 0xbea86165, 0xbf6951ad, 0xbed2b23b, 0xbf5f07cc, 0xbefb57fd, 0xbf52f9d7, 0xbf110023,
    0xbf45403c, 0xbf232e5e, 0xbf35f6ce, 0xbf3411d4, 0xbf253cba, 0xbf438821, 0xbf133355, 0xbf517269,
    0xbefffff1, 0xbf5db3dc, 0xbed791e7, 0xbf68340f, 0xbead6ee8, 0xbf70dd97, 0xbe81ec5d, 0xbf779ee3,
    0xbe2ac4ed, 0xbf7c6a42, 0xbda0adf8, 0xbf7f35fc, 0x3c2b9cea, 0xbf7ffc68, 0x3dcb69b7, 0xbf7ebbf1,
    0x3e3fe1c0, 0xbf7b7723, 0x3e8c445e, 0xbf7634ae, 0x3eb77c80, 0xbf6eff08, 0x3ee13fc7, 0xbf65e51e,
    0x3f049d79, 0xbf5af91f, 0x3f178e3f, 0xbf4e5144, 0x3f294bd1, 0xbf400735, 0x3f39b23a, 0xbf3037e9,
    0x3f48a03b, 0xbf1f036b, 0x3f55f792, 0xbf0c8c9c, 0x3f619d33, 0xbef1f1d2, 0x3f6b7969, 0xbec8e075,
    0x3f737884, 0xbe9e3701, 0x3f798a03, 0xbe649a9f, 0x3f7da1a8, 0xbe0af8d9, 0x3f7fb740, 0xbd40f19d,
    0x3f7fc683, 0x3d2b8741, 0x3f7dcf51, 0x3e05a91c, 0x3f79d5a7, 0x3e5f617a, 0x3f73e193, 0x3e9baa87,
    0x3f6bff27, 0x3ec668c8, 0x3f623e5e, 0x3eef94dd, 0x3f56b320, 0x3f0b6d72, 0x3f49746b, 0x3f1df643,
    0x3f3a9dab, 0x3f2f3e80, 0x3f2a4c80, 0x3f3f23b7, 0x3f18a223, 0x3f4d857d, 0x3f05c262, 0x3f5a46ad,
    0x3ee3a702, 0x3f654d6b, 0x3eb9fbcd, 0x3f6e835e, 0x3e8ed79b, 0x3f75d5da, 0x3e4523bd, 0x3f7b3608,
    0x3dd61155, 0x3f7e9902, 0x3c80ad1b, 0x3f7ff7ea, 0xbd95fff8, 0x3f7f4ffc, 0xbe257b99, 0x3f7ca292,
    0xbe7ea8c6, 0x3f77f50d, 0xbeaae8e1, 0x3f7150ed, 0xbed522f1, 0x3f68c3a8, 0xbefdacfa, 0x3f5e5e93,
    0xbf121a68, 0x3f5236c0, 0xbf24362d, 0x3f4464d3, 0xbf350516, 0x3f3504d0, 0xbf446513, 0x3f2435e0,
    0xbf5236f9, 0x3f121a16, 0xbf5e5ec4, 0x3efdac4d, 0xbf68c3b7, 0x3ed522b0, 0xbf71510e, 0x3eaae826,
    0xbf77f516, 0x3e7ea83d, 0xbf7ca298, 0x3e257b0d, 0xbf7f5003, 0x3d95fcdf, 0xbf7ff7ea, 0xbc80b187,
    0xbf7e98f8, 0xbdd6146c, 0xbf7b35f5, 0xbe452543, 0xbf75d5d1, 0xbe8ed7df, 0xbf6e833a, 0xbeb9fc86,
    0xbf654d5c, 0xbee3a742, 0xbf5a469b, 0xbf05c281, 0xbf4d8542, 0xbf18a272, 0xbf3f239f, 0xbf2a4c9a,
    0xbf2f3e66, 0xbf3a9dc4, 0xbf1df5f5, 0xbf4974a8, 0xbf0b6d55, 0xbf56b333, 0xbeef942d, 0xbf623e8d,
    0xbec66811, 0xbf6bff4e, 0xbe9baa43, 0xbf73e19e, 0xbe5f5ff6, 0xbf79d5bc, 0xbe05a890, 0xbf7dcf55,
    0xbd2b850b, 0xbf7fc684, 0x3d40f3d3, 0xbf7fb73e, 0x3e0af965, 0xbf7da1a3, 0x3e649c23, 0xbf7989ec,
    0x3e9e37be, 0xbf737865,
];

#[cfg(test)]
pub(crate) const CHICK_ORBIT_BASIS_BITS_FLAT_FNV1A64: u64 = 0xed587eb9c9eab1f0;

pub(crate) const CHICK_FRESH_RIDE_BOB_BITS: [u32; 35] = [
    0x00000000, 0x3bf3ad97, 0x3c6f6e0e, 0x3cae5617, 0x3cdee107, 0x3d03d347, 0x3d139d60, 0x3d1e41dd,
    0x3d2361c2, 0x3d22cf4f, 0x3d1c8fa0, 0x3d10da7d, 0x3d001862, 0x3cd5bdd2, 0x3ca3d70b, 0x3c5873a4,
    0x3bc35a39, 0xbac40989, 0xbc11d4e2, 0xbc83095f, 0xbcb896b9, 0xbce7b46c, 0xbd075ef7, 0xbd162b69,
    0xbd1fbb72, 0xbd23b9b6, 0xbd220291, 0xbd1aa556, 0xbd0de3bd, 0xbcf85f45, 0xbccc4e1e, 0xbc991d52,
    0xbc412ba7, 0xbb92c100, 0x3b43e670,
];

#[cfg(test)]
pub(crate) const CHICK_FRESH_RIDE_BOB_BITS_FNV1A64: u64 = 0x2dea517742321cce;

#[cfg(test)]
pub(crate) const NON_COURT_COLLISION_YAW_BASES: [CollisionYawBasisBits; 17] = [
    CollisionYawBasisBits::new(0x00000000, 0x3f800000, 0x00000000),
    CollisionYawBasisBits::new(0x3dcccccd, 0x3f7eb898, 0x3dcc7577),
    CollisionYawBasisBits::new(0x3e4ccccd, 0x3f7ae5a5, 0x3e4b6ff9),
    CollisionYawBasisBits::new(0x3e800000, 0x3f780aa5, 0x3e7d5777),
    CollisionYawBasisBits::new(0x3e99999a, 0x3f7490ef, 0x3e974e6d),
    CollisionYawBasisBits::new(0x3eb33333, 0x3f707abb, 0x3eaf904d),
    CollisionYawBasisBits::new(0x3f000000, 0x3f60a940, 0x3ef57744),
    CollisionYawBasisBits::new(0x3f490fdb, 0x3f3504f3, 0x3f3504f3),
    CollisionYawBasisBits::new(0x3fc90fdb, 0xb33bbd2e, 0x3f800000),
    CollisionYawBasisBits::new(0x40490fdb, 0xbf800000, 0xb3bbbd2e),
    CollisionYawBasisBits::new(0xbe19999a, 0x3f7d201a, 0xbe190650),
    CollisionYawBasisBits::new(0xbe4ccccd, 0x3f7ae5a5, 0xbe4b6ff9),
    CollisionYawBasisBits::new(0xbe800000, 0x3f780aa5, 0xbe7d5777),
    CollisionYawBasisBits::new(0xbeb33333, 0x3f707abb, 0xbeaf904d),
    CollisionYawBasisBits::new(0xbf0ccccd, 0x3f5a3f0c, 0xbf05ced5),
    CollisionYawBasisBits::new(0xbfc90fdb, 0xb33bbd2e, 0xbf800000),
    CollisionYawBasisBits::new(0xc016cbe4, 0xbf3504f3, 0xbf3504f3),
];

#[cfg(test)]
pub(crate) const NON_COURT_COLLISION_YAW_BASES_FNV1A64: u64 = 0x71fabf30c69c0a85;

#[cfg(test)]
pub(crate) const COURT_COLLISION_YAW_BASES: [CollisionYawBasisBits; 29] = [
    CollisionYawBasisBits::new(0x00000000, 0x3f800000, 0x00000000),
    CollisionYawBasisBits::new(0x3d567756, 0x3f7fa62f, 0x3d565e41),
    CollisionYawBasisBits::new(0x3e97e9d7, 0x3f74d064, 0x3e95b1bd),
    CollisionYawBasisBits::new(0x3ea0d97a, 0x3f737871, 0x3e9e3778),
    CollisionYawBasisBits::new(0x3ea0d97d, 0x3f737870, 0x3e9e377b),
    CollisionYawBasisBits::new(0x3eb2b8c3, 0x3f708fb2, 0x3eaf1d44),
    CollisionYawBasisBits::new(0x3edf66f4, 0x3f6803c9, 0x3ed8616d),
    CollisionYawBasisBits::new(0x3ee85696, 0x3f66175e, 0x3ee0722f),
    CollisionYawBasisBits::new(0x3f490fdc, 0x3f3504f2, 0x3f3504f4),
    CollisionYawBasisBits::new(0x3f567751, 0x3f2b4c24, 0x3f3e3ebe),
    CollisionYawBasisBits::new(0x3f9c61ab, 0x3eaf1d40, 0x3f708fb3),
    CollisionYawBasisBits::new(0x3fa31564, 0x3e95b1c1, 0x3f74d063),
    CollisionYawBasisBits::new(0x3fc90fda, 0x33a22169, 0x3f800000),
    CollisionYawBasisBits::new(0x4016cbe4, 0xbf3504f3, 0x3f3504f3),
    CollisionYawBasisBits::new(0x4032b8c2, 0xbf708fb2, 0x3eaf1d46),
    CollisionYawBasisBits::new(0xbe32b8c3, 0x3f7c1c5c, 0xbe31d0d5),
    CollisionYawBasisBits::new(0xbe567750, 0x3f7a67e2, 0xbe54e6ce),
    CollisionYawBasisBits::new(0xbea0d97d, 0x3f737870, 0xbe9e377b),
    CollisionYawBasisBits::new(0xbec49808, 0x3f6d5bec, 0xbebfcc6f),
    CollisionYawBasisBits::new(0xbec49809, 0x3f6d5bec, 0xbebfcc6f),
    CollisionYawBasisBits::new(0xbedf66f4, 0x3f6803c9, 0xbed8616d),
    CollisionYawBasisBits::new(0xbf1c61a9, 0x3f51b3f3, 0xbf12d5e7),
    CollisionYawBasisBits::new(0xbf490fdc, 0x3f3504f2, 0xbf3504f4),
    CollisionYawBasisBits::new(0xbf685695, 0x3f1d9bff, 0xbf49bb12),
    CollisionYawBasisBits::new(0xbf9c61ab, 0x3eaf1d40, 0xbf708fb3),
    CollisionYawBasisBits::new(0xbfc90fda, 0x33a22169, 0xbf800000),
    CollisionYawBasisBits::new(0xc016cbe4, 0xbf3504f3, 0xbf3504f3),
    CollisionYawBasisBits::new(0xc032b8c2, 0xbf708fb2, 0xbeaf1d46),
    CollisionYawBasisBits::new(0xc0490fda, 0xbf800000, 0xb4222169),
];

#[cfg(test)]
pub(crate) const COURT_COLLISION_YAW_BASES_FNV1A64: u64 = 0x38b7d3194e714575;

#[cfg(test)]
pub(crate) const CHAMPIONS_COURT_RON_FNV1A64: u64 = 0x10cf9b30c17000da;

/// Exact, symmetric relative bases for the Chick ultimate's sixteen projectiles.
///
/// Cardinal axes intentionally use exact zero rather than a platform libm
/// approximation to `cos(PI / 2)`.
pub(crate) const CHICK_ULTIMATE_RELATIVE_BASIS_BITS: [[u32; 2]; 16] = [
    [0x3f800000, 0x00000000],
    [0x3f6c835e, 0x3ec3ef15],
    [0x3f3504f3, 0x3f3504f3],
    [0x3ec3ef15, 0x3f6c835e],
    [0x00000000, 0x3f800000],
    [0xbec3ef15, 0x3f6c835e],
    [0xbf3504f3, 0x3f3504f3],
    [0xbf6c835e, 0x3ec3ef15],
    [0xbf800000, 0x00000000],
    [0xbf6c835e, 0xbec3ef15],
    [0xbf3504f3, 0xbf3504f3],
    [0xbec3ef15, 0xbf6c835e],
    [0x00000000, 0xbf800000],
    [0x3ec3ef15, 0xbf6c835e],
    [0x3f3504f3, 0xbf3504f3],
    [0x3f6c835e, 0xbec3ef15],
];

#[cfg(test)]
pub(crate) const CHICK_ULTIMATE_RELATIVE_BASIS_BITS_FNV1A64: u64 = 0x80e0_b96d_92b4_def5;

#[inline]
fn frozen_scalar(bits: &[u32], tick: u32, label: &'static str) -> f32 {
    let index = usize::try_from(tick).expect("u32 tick must fit usize");
    f32::from_bits(
        *bits
            .get(index)
            .unwrap_or_else(|| panic!("{label} tick {tick} exceeded frozen v3 domain")),
    )
}

#[inline]
pub(crate) fn bee_honey_puddle_scale(tick: u32) -> f32 {
    frozen_scalar(&BEE_HONEY_PUDDLE_SCALE_BITS, tick, "Bee honey puddle")
}

#[inline]
pub(crate) fn bee_ultimate_swarm_scale(tick: u32) -> f32 {
    frozen_scalar(&BEE_ULTIMATE_SWARM_SCALE_BITS, tick, "Bee ultimate swarm")
}

#[inline]
pub(crate) fn chick_sunny_splash_scale(tick: u32) -> f32 {
    frozen_scalar(&CHICK_SUNNY_SPLASH_SCALE_BITS, tick, "Chick sunny splash")
}

#[inline]
pub(crate) fn chick_omelet_field_scale(tick: u32) -> f32 {
    frozen_scalar(&CHICK_OMELET_FIELD_SCALE_BITS, tick, "Chick omelet field")
}

#[inline]
pub(crate) fn chick_orbit_basis(tick: u32) -> (f32, f32) {
    let index = usize::try_from(tick)
        .expect("u32 tick must fit usize")
        .checked_mul(2)
        .expect("orbit basis index must not overflow");
    let pair = CHICK_ORBIT_BASIS_BITS_FLAT
        .get(index..index + 2)
        .unwrap_or_else(|| panic!("Chick orbit tick {tick} exceeded frozen v3 domain"));
    (f32::from_bits(pair[0]), f32::from_bits(pair[1]))
}

#[inline]
pub(crate) fn chick_fresh_ride_bob(tick: u32) -> f32 {
    frozen_scalar(&CHICK_FRESH_RIDE_BOB_BITS, tick, "Chick fresh ride")
}

#[inline]
pub(crate) fn collision_yaw_basis(yaw: f32) -> (f32, f32) {
    match yaw.to_bits() {
        0x00000000 => (f32::from_bits(0x3f800000), f32::from_bits(0x00000000)),
        0x3d567756 => (f32::from_bits(0x3f7fa62f), f32::from_bits(0x3d565e41)),
        0x3dcccccd => (f32::from_bits(0x3f7eb898), f32::from_bits(0x3dcc7577)),
        0x3e4ccccd => (f32::from_bits(0x3f7ae5a5), f32::from_bits(0x3e4b6ff9)),
        0x3e800000 => (f32::from_bits(0x3f780aa5), f32::from_bits(0x3e7d5777)),
        0x3e97e9d7 => (f32::from_bits(0x3f74d064), f32::from_bits(0x3e95b1bd)),
        0x3e99999a => (f32::from_bits(0x3f7490ef), f32::from_bits(0x3e974e6d)),
        0x3ea0d97a => (f32::from_bits(0x3f737871), f32::from_bits(0x3e9e3778)),
        0x3ea0d97d => (f32::from_bits(0x3f737870), f32::from_bits(0x3e9e377b)),
        0x3eb2b8c3 => (f32::from_bits(0x3f708fb2), f32::from_bits(0x3eaf1d44)),
        0x3eb33333 => (f32::from_bits(0x3f707abb), f32::from_bits(0x3eaf904d)),
        0x3edf66f4 => (f32::from_bits(0x3f6803c9), f32::from_bits(0x3ed8616d)),
        0x3ee85696 => (f32::from_bits(0x3f66175e), f32::from_bits(0x3ee0722f)),
        0x3f000000 => (f32::from_bits(0x3f60a940), f32::from_bits(0x3ef57744)),
        0x3f490fdb => (f32::from_bits(0x3f3504f3), f32::from_bits(0x3f3504f3)),
        0x3f490fdc => (f32::from_bits(0x3f3504f2), f32::from_bits(0x3f3504f4)),
        0x3f567751 => (f32::from_bits(0x3f2b4c24), f32::from_bits(0x3f3e3ebe)),
        0x3f9c61ab => (f32::from_bits(0x3eaf1d40), f32::from_bits(0x3f708fb3)),
        0x3fa31564 => (f32::from_bits(0x3e95b1c1), f32::from_bits(0x3f74d063)),
        0x3fc90fda => (f32::from_bits(0x33a22169), f32::from_bits(0x3f800000)),
        0x3fc90fdb => (f32::from_bits(0xb33bbd2e), f32::from_bits(0x3f800000)),
        0x4016cbe4 => (f32::from_bits(0xbf3504f3), f32::from_bits(0x3f3504f3)),
        0x4032b8c2 => (f32::from_bits(0xbf708fb2), f32::from_bits(0x3eaf1d46)),
        0x40490fdb => (f32::from_bits(0xbf800000), f32::from_bits(0xb3bbbd2e)),
        0xbe19999a => (f32::from_bits(0x3f7d201a), f32::from_bits(0xbe190650)),
        0xbe32b8c3 => (f32::from_bits(0x3f7c1c5c), f32::from_bits(0xbe31d0d5)),
        0xbe4ccccd => (f32::from_bits(0x3f7ae5a5), f32::from_bits(0xbe4b6ff9)),
        0xbe567750 => (f32::from_bits(0x3f7a67e2), f32::from_bits(0xbe54e6ce)),
        0xbe800000 => (f32::from_bits(0x3f780aa5), f32::from_bits(0xbe7d5777)),
        0xbea0d97d => (f32::from_bits(0x3f737870), f32::from_bits(0xbe9e377b)),
        0xbeb33333 => (f32::from_bits(0x3f707abb), f32::from_bits(0xbeaf904d)),
        0xbec49808 => (f32::from_bits(0x3f6d5bec), f32::from_bits(0xbebfcc6f)),
        0xbec49809 => (f32::from_bits(0x3f6d5bec), f32::from_bits(0xbebfcc6f)),
        0xbedf66f4 => (f32::from_bits(0x3f6803c9), f32::from_bits(0xbed8616d)),
        0xbf0ccccd => (f32::from_bits(0x3f5a3f0c), f32::from_bits(0xbf05ced5)),
        0xbf1c61a9 => (f32::from_bits(0x3f51b3f3), f32::from_bits(0xbf12d5e7)),
        0xbf490fdc => (f32::from_bits(0x3f3504f2), f32::from_bits(0xbf3504f4)),
        0xbf685695 => (f32::from_bits(0x3f1d9bff), f32::from_bits(0xbf49bb12)),
        0xbf9c61ab => (f32::from_bits(0x3eaf1d40), f32::from_bits(0xbf708fb3)),
        0xbfc90fda => (f32::from_bits(0x33a22169), f32::from_bits(0xbf800000)),
        0xbfc90fdb => (f32::from_bits(0xb33bbd2e), f32::from_bits(0xbf800000)),
        0xc016cbe4 => (f32::from_bits(0xbf3504f3), f32::from_bits(0xbf3504f3)),
        0xc032b8c2 => (f32::from_bits(0xbf708fb2), f32::from_bits(0xbeaf1d46)),
        0xc0490fda => (f32::from_bits(0xbf800000), f32::from_bits(0xb4222169)),
        bits => panic!("unregistered canonical collision yaw bits 0x{bits:08x}"),
    }
}

/// Returns one frozen Chick-ultimate relative `(cos, sin)` basis.
#[inline]
pub(crate) fn chick_ultimate_relative_basis(index: usize) -> (f32, f32) {
    let [cos, sin] = CHICK_ULTIMATE_RELATIVE_BASIS_BITS[index];
    (f32::from_bits(cos), f32::from_bits(sin))
}

/// Fixed scalar operation order for canonical two-dimensional squared length.
#[inline(always)]
pub(crate) fn vec2_length_squared(value: Vec2) -> f32 {
    value.x * value.x + value.y * value.y
}

/// Fixed scalar operation order for canonical three-dimensional squared length.
#[inline(always)]
pub(crate) fn vec3_length_squared(value: Vec3) -> f32 {
    let xy = value.x * value.x + value.y * value.y;
    xy + value.z * value.z
}

#[inline(always)]
pub(crate) fn vec2_distance_squared(a: Vec2, b: Vec2) -> f32 {
    vec2_length_squared(Vec2::new(a.x - b.x, a.y - b.y))
}

#[inline(always)]
pub(crate) fn vec3_distance_squared(a: Vec3, b: Vec3) -> f32 {
    vec3_length_squared(Vec3::new(a.x - b.x, a.y - b.y, a.z - b.z))
}

/// Canonical square root for a squared two-dimensional magnitude.
///
/// `force-soft-floats` pins this call to libm's generic Rust implementation on
/// every supported target.
#[inline(always)]
pub(crate) fn vec2_length(value: Vec2) -> f32 {
    libm::sqrtf(vec2_length_squared(value))
}

/// Canonical square root for a squared three-dimensional magnitude.
#[inline(always)]
pub(crate) fn vec3_length(value: Vec3) -> f32 {
    libm::sqrtf(vec3_length_squared(value))
}

#[inline(always)]
pub(crate) fn vec2_distance(a: Vec2, b: Vec2) -> f32 {
    vec2_length(Vec2::new(a.x - b.x, a.y - b.y))
}

#[inline(always)]
pub(crate) fn vec3_distance(a: Vec3, b: Vec3) -> f32 {
    vec3_length(Vec3::new(a.x - b.x, a.y - b.y, a.z - b.z))
}

/// Mirrors glam's `normalize_or` invalid-input policy with canonical length.
#[inline(always)]
pub(crate) fn vec2_normalize_or(value: Vec2, fallback: Vec2) -> Vec2 {
    let reciprocal = vec2_length(value).recip();
    if reciprocal.is_finite() && reciprocal > 0.0 {
        Vec2::new(value.x * reciprocal, value.y * reciprocal)
    } else {
        fallback
    }
}

/// Mirrors glam's `normalize_or` invalid-input policy with canonical length.
#[inline(always)]
pub(crate) fn vec3_normalize_or(value: Vec3, fallback: Vec3) -> Vec3 {
    let reciprocal = vec3_length(value).recip();
    if reciprocal.is_finite() && reciprocal > 0.0 {
        Vec3::new(
            value.x * reciprocal,
            value.y * reciprocal,
            value.z * reciprocal,
        )
    } else {
        fallback
    }
}

#[inline(always)]
pub(crate) fn vec2_normalize_or_zero(value: Vec2) -> Vec2 {
    vec2_normalize_or(value, Vec2::ZERO)
}

#[inline(always)]
pub(crate) fn vec3_normalize_or_zero(value: Vec3) -> Vec3 {
    vec3_normalize_or(value, Vec3::ZERO)
}

#[cfg(test)]
pub(crate) fn fnv1a64_words(words: &[u32]) -> u64 {
    words
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yaw_words(entries: &[CollisionYawBasisBits]) -> Vec<u32> {
        entries
            .iter()
            .flat_map(|entry| [entry.yaw, entry.cos, entry.sin])
            .collect()
    }

    fn fnv1a64_bytes(bytes: &[u8]) -> u64 {
        bytes
            .iter()
            .copied()
            .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
            })
    }

    fn hash_word(hash: u64, word: u32) -> u64 {
        word.to_le_bytes().into_iter().fold(hash, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    }

    #[test]
    fn frozen_table_lengths_and_fingerprints_match_reference() {
        assert_eq!(BEE_HONEY_PUDDLE_SCALE_BITS.len(), 145);
        assert_eq!(BEE_ULTIMATE_SWARM_SCALE_BITS.len(), 145);
        assert_eq!(CHICK_SUNNY_SPLASH_SCALE_BITS.len(), 70);
        assert_eq!(CHICK_OMELET_FIELD_SCALE_BITS.len(), 124);
        assert_eq!(CHICK_ORBIT_BASIS_BITS_FLAT.len(), 962);
        assert_eq!(CHICK_FRESH_RIDE_BOB_BITS.len(), 35);
        assert_eq!(NON_COURT_COLLISION_YAW_BASES.len(), 17);
        assert_eq!(COURT_COLLISION_YAW_BASES.len(), 29);

        assert_eq!(
            fnv1a64_words(&BEE_HONEY_PUDDLE_SCALE_BITS),
            BEE_HONEY_PUDDLE_SCALE_BITS_FNV1A64
        );
        assert_eq!(
            fnv1a64_words(&BEE_ULTIMATE_SWARM_SCALE_BITS),
            BEE_ULTIMATE_SWARM_SCALE_BITS_FNV1A64
        );
        assert_eq!(
            fnv1a64_words(&CHICK_SUNNY_SPLASH_SCALE_BITS),
            CHICK_SUNNY_SPLASH_SCALE_BITS_FNV1A64
        );
        assert_eq!(
            fnv1a64_words(&CHICK_OMELET_FIELD_SCALE_BITS),
            CHICK_OMELET_FIELD_SCALE_BITS_FNV1A64
        );
        assert_eq!(
            fnv1a64_words(&CHICK_ORBIT_BASIS_BITS_FLAT),
            CHICK_ORBIT_BASIS_BITS_FLAT_FNV1A64
        );
        assert_eq!(
            fnv1a64_words(&CHICK_FRESH_RIDE_BOB_BITS),
            CHICK_FRESH_RIDE_BOB_BITS_FNV1A64
        );
        assert_eq!(
            fnv1a64_words(&yaw_words(&NON_COURT_COLLISION_YAW_BASES)),
            NON_COURT_COLLISION_YAW_BASES_FNV1A64
        );
        assert_eq!(
            fnv1a64_words(&yaw_words(&COURT_COLLISION_YAW_BASES)),
            COURT_COLLISION_YAW_BASES_FNV1A64
        );
        assert_eq!(
            fnv1a64_words(
                &CHICK_ULTIMATE_RELATIVE_BASIS_BITS
                    .iter()
                    .flatten()
                    .copied()
                    .collect::<Vec<_>>()
            ),
            CHICK_ULTIMATE_RELATIVE_BASIS_BITS_FNV1A64
        );
    }

    #[test]
    fn every_frozen_yaw_resolves_to_its_reference_basis() {
        for entry in NON_COURT_COLLISION_YAW_BASES
            .iter()
            .chain(COURT_COLLISION_YAW_BASES.iter())
        {
            let (cos, sin) = collision_yaw_basis(f32::from_bits(entry.yaw));
            assert_eq!(cos.to_bits(), entry.cos);
            assert_eq!(sin.to_bits(), entry.sin);
        }
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn c1_frozen_yaws_match_v3_reference_libm_bits() {
        const C1_REFERENCE_SIMULATION_VERSION: u16 = 3;
        for entry in NON_COURT_COLLISION_YAW_BASES
            .iter()
            .chain(COURT_COLLISION_YAW_BASES.iter())
        {
            let yaw = std::hint::black_box(f32::from_bits(entry.yaw));
            let reference = (yaw.cos().to_bits(), yaw.sin().to_bits());
            assert_eq!(
                reference.0, entry.cos,
                "cos reference mismatch for yaw bits 0x{:08x}",
                entry.yaw
            );
            assert_eq!(
                reference.1, entry.sin,
                "sin reference mismatch for yaw bits 0x{:08x}",
                entry.yaw
            );
        }
        assert_eq!(C1_REFERENCE_SIMULATION_VERSION, 3);
    }

    #[test]
    fn canonical_vector_math_has_explicit_adversarial_semantics() {
        assert_eq!(CANONICAL_MATH_REVISION, 1);
        assert_eq!(
            vec2_length(Vec2::new(3.0, 4.0)).to_bits(),
            5.0_f32.to_bits()
        );
        assert_eq!(
            vec3_length(Vec3::new(2.0, 3.0, 6.0)).to_bits(),
            7.0_f32.to_bits()
        );
        assert_eq!(
            vec2_distance(Vec2::new(-3.0, 2.0), Vec2::new(0.0, -2.0)).to_bits(),
            5.0_f32.to_bits()
        );
        assert_eq!(
            vec3_distance(Vec3::new(-2.0, 1.0, 3.0), Vec3::new(0.0, 4.0, 9.0)).to_bits(),
            7.0_f32.to_bits()
        );

        let fallback2 = Vec2::new(-7.0, 11.0);
        let fallback3 = Vec3::new(-7.0, 11.0, 13.0);
        for value in [
            Vec2::ZERO,
            Vec2::new(-0.0, 0.0),
            Vec2::splat(f32::INFINITY),
            Vec2::new(f32::NAN, 1.0),
            Vec2::splat(f32::from_bits(1)),
        ] {
            assert_eq!(vec2_normalize_or(value, fallback2), fallback2);
        }
        for value in [
            Vec3::ZERO,
            Vec3::new(-0.0, 0.0, -0.0),
            Vec3::splat(f32::INFINITY),
            Vec3::new(f32::NAN, 1.0, 2.0),
            Vec3::splat(f32::from_bits(1)),
        ] {
            assert_eq!(vec3_normalize_or(value, fallback3), fallback3);
        }
        assert_eq!(vec2_normalize_or_zero(Vec2::ZERO), Vec2::ZERO);
        assert_eq!(vec3_normalize_or_zero(Vec3::ZERO), Vec3::ZERO);
        assert!(vec2_length(Vec2::new(f32::NAN, 0.0)).is_nan());
        assert!(vec3_length(Vec3::new(f32::INFINITY, 0.0, 0.0)).is_infinite());

        let minimum_q12 = 1.0 / 4096.0;
        assert_eq!(vec2_normalize_or_zero(Vec2::new(minimum_q12, 0.0)), Vec2::X);
        assert_eq!(
            vec3_normalize_or_zero(Vec3::new(0.0, minimum_q12, 0.0)),
            Vec3::Y
        );
    }

    #[test]
    fn canonical_vector_math_q12_corpus_matches_frozen_digest() {
        let mut hash = 0xcbf2_9ce4_8422_2325;
        for x in -128_i32..=128 {
            for y in -128_i32..=128 {
                let value = Vec2::new(x as f32 / 16.0, y as f32 / 16.0);
                let normalized = vec2_normalize_or(value, Vec2::X);
                hash = hash_word(hash, vec2_length_squared(value).to_bits());
                hash = hash_word(hash, vec2_length(value).to_bits());
                hash = hash_word(hash, normalized.x.to_bits());
                hash = hash_word(hash, normalized.y.to_bits());

                let z = ((x * 73 + y * 151) & 255) - 128;
                let value3 = Vec3::new(value.x, z as f32 / 16.0, value.y);
                let normalized3 = vec3_normalize_or(value3, Vec3::Z);
                hash = hash_word(hash, vec3_length_squared(value3).to_bits());
                hash = hash_word(hash, vec3_length(value3).to_bits());
                hash = hash_word(hash, normalized3.x.to_bits());
                hash = hash_word(hash, normalized3.y.to_bits());
                hash = hash_word(hash, normalized3.z.to_bits());
            }
        }
        assert_eq!(hash, 0x74eb_67fd_4138_faa4);
    }

    #[test]
    fn champions_court_ron_digest_requires_explicit_regeneration() {
        let normalized = include_str!("../arts/champions_court.ron").replace("\r\n", "\n");
        assert_eq!(
            fnv1a64_bytes(normalized.as_bytes()),
            CHAMPIONS_COURT_RON_FNV1A64
        );
    }
}
