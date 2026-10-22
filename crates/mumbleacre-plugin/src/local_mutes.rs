//! Track only mute changes owned by ACRE so normal Mumble state can be restored.
use std::collections::HashMap;

#[derive(Clone, Copy)]
struct MuteOverride {
    original: bool,
    applied: bool,
}

#[derive(Default)]
pub(crate) struct LocalMuteOverrides {
    users: HashMap<u32, MuteOverride>,
}

impl LocalMuteOverrides {
    pub(crate) fn is_empty(&self) -> bool {
        self.users.is_empty()
    }

    pub(crate) fn apply(
        &mut self,
        user: u32,
        muted: bool,
        current: Option<bool>,
        request: impl FnOnce(bool) -> bool,
    ) -> bool {
        let Some(current) = current else {
            return false;
        };
        let original = self.users.get(&user).map_or(current, |saved| {
            if current == saved.applied {
                saved.original
            } else {
                // The user changed Mumble's state since our last request.
                current
            }
        });
        // An ACRE unmute must never remove a pre-existing manual mute.
        let desired = original || muted;
        if desired != current && !request(desired) {
            return false;
        }
        if desired == original {
            self.users.remove(&user);
        } else {
            self.users.insert(
                user,
                MuteOverride {
                    original,
                    applied: desired,
                },
            );
        }
        true
    }

    /// Failed queries/restorations remain pending for another control tick.
    /// A newer manual change is left alone instead of being overwritten.
    pub(crate) fn restore(
        &mut self,
        mut current: impl FnMut(u32) -> Option<bool>,
        mut request: impl FnMut(u32, bool) -> bool,
    ) -> usize {
        self.users.retain(|&user, saved| {
            let Some(current) = current(user) else {
                return true;
            };
            current == saved.applied && current != saved.original && !request(user, saved.original)
        });
        self.users.len()
    }

    pub(crate) fn clear(&mut self) {
        self.users.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn disconnect_restores_acre_mutes_but_preserves_manual_mutes() {
        let mut overrides = LocalMuteOverrides::default();
        let muted = Cell::new(false);
        assert!(overrides.apply(42, true, Some(muted.get()), |value| {
            muted.set(value);
            true
        }));
        assert!(muted.get());
        assert_eq!(
            overrides.restore(
                |_| Some(muted.get()),
                |id, value| {
                    assert_eq!(id, 42);
                    muted.set(value);
                    true
                }
            ),
            0
        );
        assert!(!muted.get());

        assert!(overrides.apply(43, false, Some(true), |_| panic!("manual mute changed")));
        assert_eq!(
            overrides.restore(|_| panic!("nothing to restore"), |_, _| false),
            0
        );
    }

    #[test]
    fn newer_manual_changes_and_failed_requests_are_not_rolled_back() {
        let mut overrides = LocalMuteOverrides::default();
        assert!(!overrides.apply(42, true, Some(false), |_| false));
        assert_eq!(
            overrides.restore(|_| panic!("failed request was not applied"), |_, _| false),
            0
        );
        assert!(overrides.apply(42, true, Some(false), |_| true));
        assert_eq!(
            overrides.restore(|_| Some(false), |_, _| panic!("manual change overwritten")),
            0
        );
    }

    #[test]
    fn failed_restore_can_be_retried_and_acre_unmute_releases_its_override() {
        let mut overrides = LocalMuteOverrides::default();
        assert!(overrides.apply(42, true, Some(false), |_| true));
        assert_eq!(overrides.restore(|_| None, |_, _| true), 1);
        assert_eq!(overrides.restore(|_| Some(true), |_, _| false), 1);
        assert_eq!(overrides.restore(|_| Some(true), |_, muted| !muted), 0);
        assert!(overrides.apply(42, true, Some(false), |_| true));
        assert!(overrides.apply(42, false, Some(true), |muted| !muted));
        assert_eq!(
            overrides.restore(|_| panic!("override already released"), |_, _| false),
            0
        );
    }
}
