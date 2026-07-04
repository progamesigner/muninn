//! The server-wide policy and the two-region permission model.
//!
//! There are exactly two regions per server instance: *inside the agents folder*
//! (scoped, suffix-applied) and *outside the agents folder but inside the vault
//! root* (shared, no suffix). A single [`Policy`] governs the read/write
//! permissions in each region.

/// The region a resolved path falls into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    /// Inside the configured agents folder — scoped and (with a non-empty
    /// scheme) suffix-applied.
    InsideAgentsFolder,
    /// Outside the agents folder but still inside the vault root — shared, no
    /// suffix.
    OutsideAgentsFolder,
}

/// The server-wide policy (`MUNINN_POLICY`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Policy {
    /// Inside: own-scope R+W. Outside: denied.
    Scoped,
    /// Inside: own-scope R+W. Outside: read-only. (default)
    #[default]
    Namespaced,
    /// Inside: own-scope read-only. Outside: read-only. No writes anywhere.
    Readonly,
    /// Inside: own-scope R+W. Outside: R+W.
    Readwrite,
}

/// The read/write permissions available in a region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permission {
    pub read: bool,
    pub write: bool,
}

/// The reason a policy gate refused an operation, before a virtual path is
/// attached to form an [`crate::error::MuninnError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyError {
    /// The region is entirely unreachable under the active policy.
    NotPermitted,
    /// The region is readable but not writable under the active policy.
    WriteDenied,
}

impl Policy {
    /// Parse the policy from its env-var string form.
    pub fn parse(s: &str) -> Option<Policy> {
        match s {
            "scoped" => Some(Policy::Scoped),
            "namespaced" => Some(Policy::Namespaced),
            "readonly" => Some(Policy::Readonly),
            "readwrite" => Some(Policy::Readwrite),
            _ => None,
        }
    }

    /// The accepted string values, for error messages.
    pub const ACCEPTED: &'static [&'static str] =
        &["scoped", "namespaced", "readonly", "readwrite"];

    /// The permissions available in `region` under this policy.
    pub fn permission_for(self, region: Region) -> Permission {
        use Policy::*;
        use Region::*;
        match (self, region) {
            (Scoped, InsideAgentsFolder) => Permission {
                read: true,
                write: true,
            },
            (Scoped, OutsideAgentsFolder) => Permission {
                read: false,
                write: false,
            },
            (Namespaced, InsideAgentsFolder) => Permission {
                read: true,
                write: true,
            },
            (Namespaced, OutsideAgentsFolder) => Permission {
                read: true,
                write: false,
            },
            (Readonly, InsideAgentsFolder) => Permission {
                read: true,
                write: false,
            },
            (Readonly, OutsideAgentsFolder) => Permission {
                read: true,
                write: false,
            },
            (Readwrite, InsideAgentsFolder) => Permission {
                read: true,
                write: true,
            },
            (Readwrite, OutsideAgentsFolder) => Permission {
                read: true,
                write: true,
            },
        }
    }

    /// Gate a read against the policy. Every tool handler calls this before IO.
    pub fn gate_read(self, region: Region) -> std::result::Result<(), PolicyError> {
        if self.permission_for(region).read {
            Ok(())
        } else {
            Err(PolicyError::NotPermitted)
        }
    }

    /// Gate a write against the policy. Every tool handler calls this before IO.
    ///
    /// A fully-denied region (no read, no write — i.e. `scoped` outside) yields
    /// [`PolicyError::NotPermitted`]; a readable-but-not-writable region yields
    /// [`PolicyError::WriteDenied`].
    pub fn gate_write(self, region: Region) -> std::result::Result<(), PolicyError> {
        let perm = self.permission_for(region);
        if perm.write {
            Ok(())
        } else if !perm.read {
            Err(PolicyError::NotPermitted)
        } else {
            Err(PolicyError::WriteDenied)
        }
    }

    /// The regions `list_memory_notes` should walk, in deterministic order.
    ///
    /// The inside-agents-folder region is always readable; the outside region is
    /// only readable when the policy permits it. The empty-scheme case does not
    /// change the *set* of regions (it only removes own-scope filtering, which the
    /// storage layer handles), so the parameter is accepted for API completeness.
    pub fn list_visible_regions(self, _scheme_is_empty: bool) -> Vec<Region> {
        let mut regions = vec![Region::InsideAgentsFolder];
        if self.permission_for(Region::OutsideAgentsFolder).read {
            regions.push(Region::OutsideAgentsFolder);
        }
        regions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Region::*;

    #[test]
    fn parse_accepts_the_four_policies_and_rejects_others() {
        assert_eq!(Policy::parse("scoped"), Some(Policy::Scoped));
        assert_eq!(Policy::parse("namespaced"), Some(Policy::Namespaced));
        assert_eq!(Policy::parse("readonly"), Some(Policy::Readonly));
        assert_eq!(Policy::parse("readwrite"), Some(Policy::Readwrite));
        assert_eq!(Policy::parse("nonsense"), None);
        assert_eq!(Policy::parse(""), None);
    }

    #[test]
    fn default_is_namespaced() {
        assert_eq!(Policy::default(), Policy::Namespaced);
    }

    /// scoped denies the outside region with `path_not_permitted` semantics.
    #[test]
    fn scoped_denies_outside_region() {
        assert_eq!(
            Policy::Scoped.gate_read(OutsideAgentsFolder),
            Err(PolicyError::NotPermitted)
        );
        assert_eq!(
            Policy::Scoped.gate_write(OutsideAgentsFolder),
            Err(PolicyError::NotPermitted)
        );
        assert!(Policy::Scoped.gate_read(InsideAgentsFolder).is_ok());
        assert!(Policy::Scoped.gate_write(InsideAgentsFolder).is_ok());
    }

    /// namespaced permits reads outside but denies writes there with `write_denied`.
    #[test]
    fn namespaced_reads_outside_but_denies_writes() {
        assert!(Policy::Namespaced.gate_read(OutsideAgentsFolder).is_ok());
        assert_eq!(
            Policy::Namespaced.gate_write(OutsideAgentsFolder),
            Err(PolicyError::WriteDenied)
        );
        assert!(Policy::Namespaced.gate_write(InsideAgentsFolder).is_ok());
    }

    /// readonly forbids writes everywhere with `write_denied`.
    #[test]
    fn readonly_forbids_writes_everywhere() {
        assert_eq!(
            Policy::Readonly.gate_write(InsideAgentsFolder),
            Err(PolicyError::WriteDenied)
        );
        assert_eq!(
            Policy::Readonly.gate_write(OutsideAgentsFolder),
            Err(PolicyError::WriteDenied)
        );
        assert!(Policy::Readonly.gate_read(InsideAgentsFolder).is_ok());
        assert!(Policy::Readonly.gate_read(OutsideAgentsFolder).is_ok());
    }

    /// readwrite permits writes in both regions.
    #[test]
    fn readwrite_permits_writes_outside() {
        assert!(Policy::Readwrite.gate_write(OutsideAgentsFolder).is_ok());
        assert!(Policy::Readwrite.gate_write(InsideAgentsFolder).is_ok());
    }

    #[test]
    fn visible_regions_track_outside_readability() {
        assert_eq!(
            Policy::Scoped.list_visible_regions(false),
            vec![InsideAgentsFolder]
        );
        assert_eq!(
            Policy::Namespaced.list_visible_regions(false),
            vec![InsideAgentsFolder, OutsideAgentsFolder]
        );
        assert_eq!(
            Policy::Readwrite.list_visible_regions(true),
            vec![InsideAgentsFolder, OutsideAgentsFolder]
        );
    }
}
