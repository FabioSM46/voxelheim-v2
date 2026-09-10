use super::*;

#[test]
fn gait_audio_marks_finished_supports_but_never_corrections_or_attack_poses() {
    let dt = Duration::from_secs_f32(1.0 / 60.0);
    let mut motion = Motion::new(Vec3::ZERO, 0.0);
    let mut marked = 0;
    for frame in 0..120 {
        let position = Vec3::new(0.0, 0.0, -(frame as f32) / 60.0);
        let serial = motion.audio_serial;
        motion.sample(position, 0.0, MobAction::Chase, dt * frame, 0.0, dt);
        if let Some(contact) = motion.audio_contact {
            assert_eq!(motion.audio_serial, serial.wrapping_add(1));
            assert!(contact.y.abs() < 0.001);
            marked += 1;
        } else {
            assert_eq!(motion.audio_serial, serial);
        }
    }
    assert!(
        marked > 3 && marked < 60,
        "paired supports, not one sound per frame: {marked}"
    );
    let serial = motion.audio_serial;
    motion.sample(
        Vec3::new(20.0, 4.0, 0.0),
        2.0,
        MobAction::Chase,
        dt,
        0.0,
        dt,
    );
    assert_eq!(motion.audio_serial, serial);
    assert!(motion.audio_contact.is_none());
    for action in [MobAction::Windup, MobAction::Recovery, MobAction::Corpse] {
        motion.sample(Vec3::new(20.0, 4.0, -0.1), 2.0, action, dt, 0.0, dt);
        assert!(motion.audio_contact.is_none());
    }
}
