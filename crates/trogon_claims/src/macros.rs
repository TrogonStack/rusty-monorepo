#[macro_export]
macro_rules! object_id {
    ($literal:literal) => {
        $crate::__private::object_id_impl!($crate, $literal)
    };
}

#[macro_export]
macro_rules! object_type {
    ($literal:literal) => {
        $crate::__private::object_type_impl!($crate, $literal)
    };
}
