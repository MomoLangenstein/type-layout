use crate::{
    typeset::{ComputeTypeSet, ExpandTypeSet, Set},
    TypeLayout, TypeLayoutInfo, TypeStructure,
};

unsafe impl TypeLayout for core::ffi::c_void {
    const TYPE_LAYOUT: TypeLayoutInfo<'static> = TypeLayoutInfo {
        name: ::core::any::type_name::<Self>(),
        size: ::core::mem::size_of::<Self>(),
        alignment: ::core::mem::align_of::<Self>(),
        structure: TypeStructure::Enum {
            repr: "u8",
            variants: &[],
        },
    };
}

unsafe impl ComputeTypeSet for core::ffi::c_void {
    type Output<T: ExpandTypeSet> = Set<Self, T>;
}
