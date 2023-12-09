use crate::{
    typeset::{ComputeTypeSet, ExpandTypeSet, Set},
    TypeLayout, TypeLayoutInfo, TypeStructure,
};

unsafe impl<T> TypeLayout for core::marker::PhantomData<T> {
    const TYPE_LAYOUT: TypeLayoutInfo<'static> = TypeLayoutInfo {
        name: ::core::any::type_name::<Self>(),
        size: ::core::mem::size_of::<Self>(),
        alignment: ::core::mem::align_of::<Self>(),
        structure: TypeStructure::Struct {
            repr: "",
            fields: &[],
        },
    };
}

unsafe impl<T> ComputeTypeSet for core::marker::PhantomData<T> {
    type Output<R: ExpandTypeSet> = Set<Self, R>;
}

unsafe impl TypeLayout for core::marker::PhantomPinned {
    const TYPE_LAYOUT: TypeLayoutInfo<'static> = TypeLayoutInfo {
        name: ::core::any::type_name::<Self>(),
        size: ::core::mem::size_of::<Self>(),
        alignment: ::core::mem::align_of::<Self>(),
        structure: TypeStructure::Struct {
            repr: "",
            fields: &[],
        },
    };
}

unsafe impl ComputeTypeSet for core::marker::PhantomPinned {
    type Output<T: ExpandTypeSet> = Set<Self, T>;
}
