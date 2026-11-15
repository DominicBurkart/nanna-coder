use model::default_model::*;
use proptest::prelude::*;

fn variants_strategy() -> impl Strategy<Value = Vec<ModelVariant>> {
    proptest::collection::vec(
        (1u64..200_000_000_000, 1u64..(1u64 << 40)).prop_map(|(params, min)| ModelVariant {
            tag: format!("gemma4:{params}-{min}"),
            params,
            min_memory_bytes: min,
        }),
        0..12,
    )
}

fn accel_strategy() -> impl Strategy<Value = Accel> {
    prop_oneof![
        Just(Accel::Cpu),
        Just(Accel::Cuda),
        Just(Accel::Rocm),
        Just(Accel::Metal)
    ]
}

proptest! {
    #[test]
    fn selection_is_monotone_in_capacity(
        variants in variants_strategy(),
        ram in 0u64..(1u64 << 41),
        vram in proptest::option::of(0u64..(1u64 << 41)),
        extra_ram in 0u64..(1u64 << 40),
        extra_vram in 0u64..(1u64 << 40),
        accel in accel_strategy(),
    ) {
        let small = DeviceCapacity { ram_bytes: ram, vram_bytes: vram, accel };
        let big = DeviceCapacity {
            ram_bytes: ram + extra_ram,
            vram_bytes: vram.map(|v| v + extra_vram),
            accel,
        };
        let a = select_default(&small, &variants).map(|v| v.params);
        let b = select_default(&big, &variants).map(|v| v.params);
        match (a, b) {
            (Some(a), Some(b)) => prop_assert!(b >= a),
            (Some(_), None) => prop_assert!(false, "bigger device lost its model"),
            _ => {}
        }
    }

    #[test]
    fn selected_variant_always_fits_with_headroom_and_is_the_largest_that_does(
        variants in variants_strategy(),
        ram in 0u64..(1u64 << 41),
        vram in proptest::option::of(0u64..(1u64 << 41)),
        accel in accel_strategy(),
    ) {
        let cap = DeviceCapacity { ram_bytes: ram, vram_bytes: vram, accel };
        let usable = if accel == Accel::Cpu { ram } else { vram.unwrap_or(ram) } as u128;
        let fits = |v: &ModelVariant| (v.min_memory_bytes as u128) * 6 <= usable * 5;
        match select_default(&cap, &variants) {
            Some(chosen) => {
                prop_assert!(fits(&chosen));
                prop_assert!(variants.iter().filter(|v| fits(v)).all(|v| v.params <= chosen.params));
            }
            None => prop_assert!(variants.iter().all(|v| !fits(v))),
        }
    }

    #[test]
    fn parse_capacity_never_panics(text in ".{0,200}") {
        let _ = parse_capacity(&text, None);
    }

    #[test]
    fn parse_params_never_panics(text in ".{0,40}") {
        let _ = parse_params(&text);
    }
}
