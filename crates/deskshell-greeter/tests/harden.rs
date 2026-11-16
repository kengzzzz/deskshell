//! The greeter turns itself non-dumpable, alone in this file so the change stays in its process.

use rustix::process::{DumpableBehavior, dumpable_behavior};

#[test]
fn the_greeter_is_not_dumpable() {
    deskshell_greeter::harden().unwrap();
    assert_eq!(
        dumpable_behavior().unwrap(),
        DumpableBehavior::NotDumpable
    );
}
