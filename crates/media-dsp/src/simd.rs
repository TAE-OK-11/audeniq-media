//! Runtime selection of the AVX2 build of a kernel on x86-64.
//!
//! Kernels are written once as safe, `#[inline(always)]` Rust. [`multiversion`]
//! wraps one in a function that, on x86-64, also compiles the body with AVX2
//! and FMA enabled and calls that copy when the CPU has them; elsewhere (and
//! on AArch64, where NEON is baseline and the body is vectorized for it) the
//! plain body runs. Rust never contracts `a * b + c` into an FMA by itself,
//! so both copies compute the same floats.

/// AVX2 and FMA present (checked once; `std` caches CPUID).
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn avx2() -> bool {
    std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma")
}

/// The vector instruction set the kernels run with on this machine.
pub fn level() -> &'static str {
    #[cfg(target_arch = "x86_64")]
    if avx2() {
        return "avx2";
    }
    #[cfg(target_arch = "aarch64")]
    return "neon";
    #[allow(unreachable_code)]
    "baseline"
}

/// `fn name(args) -> ret = generic;` defines `name`, dispatching to an AVX2
/// build of `generic` when available.
macro_rules! multiversion {
    ($(#[$m:meta])* $vis:vis fn $name:ident($($arg:ident: $ty:ty),* $(,)?) $(-> $ret:ty)? = $generic:path;) => {
        $(#[$m])*
        #[allow(unsafe_code)]
        $vis fn $name($($arg: $ty),*) $(-> $ret)? {
            #[cfg(target_arch = "x86_64")]
            {
                #[target_feature(enable = "avx2,fma")]
                fn avx2($($arg: $ty),*) $(-> $ret)? {
                    $generic($($arg),*)
                }
                if $crate::simd::avx2() {
                    // SAFETY: AVX2 and FMA were detected on this CPU.
                    return unsafe { avx2($($arg),*) };
                }
            }
            $generic($($arg),*)
        }
    };
}
pub(crate) use multiversion;
