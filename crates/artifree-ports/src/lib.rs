//! Outbound dependency boundary. Concrete SDKs and database drivers belong in
//! `artifree-providers`. Operation signatures will be introduced with their first
//! behavior contract, including cancellation, errors, and commit semantics.

/// Capability families; listing a family does not grant runtime permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderFamily {
    Storage,
    Models,
    Tools,
    Build,
    Export,
}

pub const PROVIDER_FAMILIES: [ProviderFamily; 5] = [
    ProviderFamily::Storage,
    ProviderFamily::Models,
    ProviderFamily::Tools,
    ProviderFamily::Build,
    ProviderFamily::Export,
];
