//! Every combat, guardian and king cue as the palette bakes it, pinned bit for bit (see
//! `audio::synth::pin`).
use super::*;
use crate::audio::synth::pin;

#[test]
fn every_combat_sound_bakes_bit_identical_to_its_pin() {
    let rendered: Vec<_> = sounds::CUES
        .iter()
        .copied()
        .chain(guardian::sounds::CUES.into_iter().map(Cue::Guardian))
        .chain(king::sounds::CUES.into_iter().map(Cue::King))
        .map(|cue| {
            // The palette's own seed, in `update`.
            let hash = pin::baked(&cue.describe(), cue.seconds(), 19);
            (format!("{cue:?}"), hash)
        })
        .collect();
    pin::assert_pins(&rendered, PINS);
}

const PINS: &[(&str, u64)] = &[
    ("DryImpact", 0xd402ff5959e9dd0c),
    ("BeastImpact", 0xe1e61b4f29ddc5df),
    ("ClothImpact", 0xb4ba63231171f4c4),
    ("SoftImpact", 0xb2a628d0e73ab4f9),
    ("DraugrNotice", 0x646492d88fdcbe3e),
    ("DraugrAttack", 0x849dc2440a4457e7),
    ("VargrNotice", 0xb42f68db21156849),
    ("VargrAttack", 0x069016b87c637d71),
    ("Guardian(Notice)", 0x2fccd599ddf46819),
    ("Guardian(BiteLoad)", 0xcfbc270b41aff5f5),
    ("Guardian(BiteLoadSecond)", 0x1b6390f0ed9e6e44),
    ("Guardian(BiteSnap)", 0x30437410ea3c0b46),
    ("Guardian(BiteTear)", 0xa77ee4ea9c5b6071),
    ("Guardian(ClawLift)", 0xa42b602595102b4e),
    ("Guardian(ClawLeft)", 0x9b65f304339a5451),
    ("Guardian(ClawRight)", 0x05fe676ac6743115),
    ("Guardian(PawSettle)", 0x5548106811b5c595),
    ("Guardian(Scrape)", 0xdc03d6d2d1548b71),
    ("Guardian(ChainTaut)", 0x8e0dcf6e6b94fa3b),
    ("Guardian(ChainRun)", 0x2f1bbd9b0dcd58d7),
    ("Guardian(ChargeStop)", 0x804c242cd4d01b94),
    ("Guardian(LeapLoad)", 0x09bcddb1eb6747a0),
    ("Guardian(LeapLaunch)", 0xd4a7082f46c52496),
    ("Guardian(Landing)", 0xff3ceee958b529a7),
    ("Guardian(JawsLoad)", 0xa1a0820c187c8ab8),
    ("Guardian(JawsClose)", 0x4dca9609d62ef3cb),
    ("Guardian(Recovery)", 0x11dea3affd5ab4fa),
    ("Guardian(StrapTear)", 0x6bf34c3159e81d9c),
    ("Guardian(Death)", 0xa982e4e3c0763151),
    ("Guardian(Footfall)", 0xad4ad2ce4bff774b),
    ("King(Notice)", 0x3c8e77ca4916701c),
    ("King(SentenceRaise)", 0xcc85a3b16bbabc7a),
    ("King(SentenceClang)", 0x7680a17900f02ea6),
    ("King(SentenceCut)", 0x3da4db05ad9ebf72),
    ("King(BladeBite)", 0x0050fc96adcaecb1),
    ("King(BladeFree)", 0x722afb1d6e3d1587),
    ("King(TollFirst)", 0x69345205573c6f73),
    ("King(TollSecond)", 0x730070aee4c62490),
    ("King(TollThird)", 0xce4d4307818448c8),
    ("King(SweepLeft)", 0x7088e1bec765e50e),
    ("King(SweepRight)", 0x6457a91a27b8906f),
    ("King(Thrust)", 0x54a5d6d286c53b8d),
    ("King(Recovery)", 0x385bba6df6da2e13),
    ("King(MaskFall)", 0x1b9c548b5967a441),
    ("King(Death)", 0xd963b9b1a6acf2f7),
    ("King(SpearGather)", 0x9f61d5d631a81e9e),
    ("King(SpearLoose)", 0x49e86076ea8d6ba7),
    ("King(Plant)", 0xf08ddc10d6d6b473),
    ("King(CracksRun)", 0xcd6558035cbb0ef7),
    ("King(BurialErupt)", 0x646c85e6075515a4),
    ("King(EdictCall)", 0x1defa92b5a069167),
    ("King(RuneFirst)", 0x2603916772efaf05),
    ("King(RuneSecond)", 0xee2e4737c5748b9f),
    ("King(RuneThird)", 0xd24398e7798755ab),
    ("King(GravesErupt)", 0xd2f4946cfd642d5f),
    ("King(NoteFirst)", 0x74fdbe6fd7791f05),
    ("King(NoteSecond)", 0xd1aea4cc9b3caf34),
    ("King(NoteThird)", 0xeda2d6fc5ea54711),
    ("King(RequiemToll)", 0x5642a317b9323a30),
    ("King(ChantBroken)", 0x84972bd7e2d74d52),
];
