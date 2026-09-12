//! Every combat, guardian and king cue as the palette bakes it, pinned (see
//! `audio::synth::pin`).
use super::*;
use crate::audio::synth::pin;

#[test]
fn every_combat_sound_bakes_identically_to_its_pin() {
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
    ("DryImpact", 0xfb2ef3bf6795e8a7),
    ("BeastImpact", 0x2c3811b95bcb635d),
    ("ClothImpact", 0xdb93d8e615b051cb),
    ("SoftImpact", 0xef1e75e3a0696345),
    ("DraugrNotice", 0x233658520abdf3dd),
    ("DraugrAttack", 0x48cd4d49e1d0dde2),
    ("VargrNotice", 0xca204f4f2f89ff46),
    ("VargrAttack", 0x8e1de51814a60bb3),
    ("Guardian(Notice)", 0xa0dd9a789d4eefe4),
    ("Guardian(BiteLoad)", 0xdee39e64b9a149dc),
    ("Guardian(BiteLoadSecond)", 0xedd510026440f933),
    ("Guardian(BiteSnap)", 0x7aac08434328e7a6),
    ("Guardian(BiteTear)", 0xc1639cfec1bfa361),
    ("Guardian(ClawLift)", 0x74a04b4f1fc42ab2),
    ("Guardian(ClawLeft)", 0xb4df77d2ae5d3ffe),
    ("Guardian(ClawRight)", 0x166d8002314f9f0a),
    ("Guardian(PawSettle)", 0x526251ef7c1693bd),
    ("Guardian(Scrape)", 0x293dd803587930f3),
    ("Guardian(ChainTaut)", 0x31af263e2e71b9e9),
    ("Guardian(ChainRun)", 0x5d1bdb032b26b315),
    ("Guardian(ChargeStop)", 0xe67493c18e0d6da4),
    ("Guardian(LeapLoad)", 0x1a15dd0b9fb3ef9f),
    ("Guardian(LeapLaunch)", 0xbbe687c02161ac9f),
    ("Guardian(Landing)", 0xb021716ff0c40a5d),
    ("Guardian(JawsLoad)", 0xa5c42b4df4e085d5),
    ("Guardian(JawsClose)", 0xe20dc11e0b36fe72),
    ("Guardian(Recovery)", 0xdd2ed877f3a514da),
    ("Guardian(StrapTear)", 0xe8fe070fbe559087),
    ("Guardian(Death)", 0xddb03b0e452b4911),
    ("Guardian(Footfall)", 0xc7840289f9e353a8),
    ("King(Notice)", 0xb47c5fd89c1b0eca),
    ("King(SentenceRaise)", 0xab585d90d484a04b),
    ("King(SentenceClang)", 0x675f9cd0ad807781),
    ("King(SentenceCut)", 0x32d7f83b4c0a3e6b),
    ("King(BladeBite)", 0x62829e85674b9176),
    ("King(BladeFree)", 0xb37ffc5f3ecb673e),
    ("King(TollFirst)", 0xa0dcdd9ef2d77e43),
    ("King(TollSecond)", 0x69782e3b28876891),
    ("King(TollThird)", 0xa1e2f8f849162bfd),
    ("King(SweepLeft)", 0xf837950a845d3973),
    ("King(SweepRight)", 0x740467ee0c56b3a7),
    ("King(Thrust)", 0x33557febeaf9981d),
    ("King(Recovery)", 0x54878d81c7fc3279),
    ("King(MaskFall)", 0x093e06532c424144),
    ("King(Death)", 0x1eb650c91fb1f2d1),
    ("King(SpearGather)", 0x4a63ba0cf3dd9724),
    ("King(SpearLoose)", 0xf10a92528899bb2a),
    ("King(Plant)", 0x9fd617dbd21c3335),
    ("King(CracksRun)", 0x3ecf16b3c1aa9935),
    ("King(BurialErupt)", 0xe3e721207fa66059),
    ("King(EdictCall)", 0x812d5150cfeab1bd),
    ("King(RuneFirst)", 0x1db15e6b4cbfb463),
    ("King(RuneSecond)", 0x3cd0ecbb890d35c5),
    ("King(RuneThird)", 0x8a298861546ecf73),
    ("King(GravesErupt)", 0x1dcf80a5be954a72),
    ("King(NoteFirst)", 0x5704cd70aeb00089),
    ("King(NoteSecond)", 0xd6d8cd0f1a3f1d56),
    ("King(NoteThird)", 0x938c99791e40c6bf),
    ("King(RequiemToll)", 0x481e3e3f456a00fa),
    ("King(ChantBroken)", 0xefe711c053d03722),
];
