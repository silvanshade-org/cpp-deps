//! Native build-script smoke fixture linking C++20 modules into a Rust test.

#[cfg(test)]
mod tests
{
    unsafe extern "C" {
        fn module_answer() -> core::ffi::c_int;
        fn implementation_value() -> core::ffi::c_int;
    }

    #[test]
    fn imported_interface_and_implementation_link_into_rust()
    {
        // SAFETY: build.rs links the importer with the matching C signature.
        let imported = unsafe { module_answer() };
        // SAFETY: build.rs links the implementation with the matching C signature.
        let implemented = unsafe { implementation_value() };
        assert_eq!(
            imported, 42_i32,
            "consumer must import partition through interface"
        );
        assert_eq!(
            implemented, 42_i32,
            "module implementation must see its interface"
        );
    }
}
