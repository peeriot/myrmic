use std::collections::BTreeSet;
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use super::Host;

/// the [`Host::name`]s some test of this process holds
static HELD: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
/// notified whenever hosts are released from [`HELD`]
static RELEASED: Condvar = Condvar::new();

const POISONED: &str = "the host lock table is never left poisoned: nothing panics holding it";

/// Holds `hosts` against every other test of this process until the returned guard is dropped,
/// waiting up to `timeout` for hosts another test holds. Panics, naming them, if they stay held.
///
/// The tests of one test binary run in parallel threads, and two of them on one host would
/// interfere. All hosts are taken at once, so two tests waiting for each other's hosts cannot
/// deadlock. Other processes are not covered: test binaries run one after another, and in CI the
/// runner runs one job at a time.
pub fn lock(hosts: &[&Host], timeout: Duration) -> HostLocks {
    // a set: a host given twice (e.g. the entry host also running a runtime) is held once
    let names: BTreeSet<String> = hosts.iter().map(|host| host.name.clone()).collect();
    let held = HELD.lock().expect(POISONED);
    let (mut held, wait) = RELEASED
        .wait_timeout_while(held, timeout, |held| !held.is_disjoint(&names))
        .expect(POISONED);
    if wait.timed_out() {
        let busy: Vec<&String> = names.intersection(&held).collect();
        let message = format!("hosts {busy:?} still held by another test after {timeout:?}");
        // released before panicking, so the panic doesn't poison the table for the other tests
        drop(held);
        panic!("{message}");
    }
    held.extend(names.iter().cloned());
    HostLocks { names }
}

/// The hosts a [`lock`] holds, released on drop.
pub struct HostLocks {
    names: BTreeSet<String>,
}

impl Drop for HostLocks {
    fn drop(&mut self) {
        let mut held = HELD.lock().expect(POISONED);
        for name in &self.names {
            let was_held = held.remove(name);
            assert!(was_held, "host {name} was released while not held");
        }
        drop(held);
        RELEASED.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::{Host, lock};

    /// a host no other test locks: the table is shared by every test of this binary
    fn host(name: &str) -> Host {
        Host {
            name: name.to_owned(),
            login: format!("user@{name}"),
            bin_dir: "/bin".to_owned(),
        }
    }

    #[test]
    fn a_held_host_is_taken_once_released() {
        let (a, b) = (host("lock-test-released-a"), host("lock-test-released-b"));
        let first = lock(&[&a], Duration::from_secs(1));
        let (taken_tx, taken) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let _second = lock(&[&b, &a], Duration::from_secs(10));
                taken_tx.send(()).expect("the test waits for this");
            });
            assert!(
                taken.recv_timeout(Duration::from_millis(200)).is_err(),
                "a host was taken while another test held it"
            );
            drop(first);
            taken
                .recv_timeout(Duration::from_secs(5))
                .expect("the released host was not taken");
        });
    }

    #[test]
    #[should_panic(expected = "lock-test-timeout")]
    fn a_host_held_past_the_timeout_panics_naming_it() {
        let busy = host("lock-test-timeout");
        let _first = lock(&[&busy], Duration::from_secs(1));
        lock(&[&busy], Duration::from_millis(50));
    }
}
