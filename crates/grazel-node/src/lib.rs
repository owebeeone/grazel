//! grazel-node — placeholder for the iroh-era node services (PublicSurfaces §1b).
//!
//! Deliberately undesigned (GrazelWorkstream §0): iroh is coming, not discussed;
//! nothing here may bake in an assumption that contradicts per-scope identity
//! (§1e — each scope owns its iroh secret key; there is no per-user identity).
//! The crate exists so the dependency arrow (`grazel-cli-lib` → `grazel-node`)
//! is pinned from day one; it grows a real API when the iroh arc opens.

/// The crate links; nothing else is promised yet.
pub const PLACEHOLDER: &str = "grazel-node: iroh arc not open";

#[cfg(test)]
mod tests {
    #[test]
    fn placeholder_links() {
        assert!(super::PLACEHOLDER.contains("iroh"));
    }
}
