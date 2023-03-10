/// Provides methods for generating types from the [`differential_dataflow`] crate for use with [`proptest`]
pub mod differential {
    use differential_dataflow::trace::Description;
    use proptest::{prelude::Arbitrary, strategy::Strategy};
    use timely::progress::Antichain;

    /// Returns a [`Strategy`] for generating [`Description`]s
    pub fn description_strategy<T>() -> impl Strategy<Value = Description<T>>
    where
        T: Arbitrary + timely::order::PartialOrder + Clone,
    {
        let lower = proptest::collection::vec(proptest::arbitrary::any::<T>(), 1..32)
            .prop_map(|v| Antichain::from(v));
        let upper = proptest::collection::vec(proptest::arbitrary::any::<T>(), 0..32)
            .prop_map(|v| Antichain::from(v));
        let since = proptest::collection::vec(proptest::arbitrary::any::<T>(), 0..32)
            .prop_map(|v| Antichain::from(v));

        (lower, upper, since).prop_map(|(l, u, s)| Description::new(l, u, s))
    }
}

/// Provides methods for generating types from the [`semver`] crate for use with [`proptest`]
pub mod semver {
    use proptest::strategy::Strategy;

    /// Returns a [`Strategy`] for generating [`semver::Version`]s
    pub fn version_strategy() -> impl Strategy<Value = semver::Version> {
        (
            proptest::arbitrary::any::<u64>(),
            proptest::arbitrary::any::<u64>(),
            proptest::arbitrary::any::<u64>(),
        )
            .prop_map(|(major, minor, patch)| semver::Version::new(major, minor, patch))
    }
}

/// Provides methods for generating types from the [`timely`] crate for use with [`proptest`]
pub mod timely {
    use proptest::arbitrary::Arbitrary;
    use proptest::strategy::Strategy;
    use timely::progress::Antichain;

    /// Returns a [`Strategy`] for generating [`Antichain`]s
    pub fn antichain_strategy<T: Arbitrary + timely::order::PartialOrder>(
    ) -> impl Strategy<Value = Antichain<T>> {
        proptest::collection::vec(proptest::arbitrary::any::<T>(), 0..128)
            .prop_map(|v| Antichain::from(v))
    }
}
