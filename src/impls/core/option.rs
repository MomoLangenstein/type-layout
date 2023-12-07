use crate::{
    typeset::{tset, ComputeTypeSet, ExpandTypeSet, Set},
    Field, MaybeUninhabited, TypeGraph, TypeLayout, TypeLayoutGraph, TypeLayoutInfo, TypeStructure,
    Variant,
};

unsafe impl<T: ~const TypeLayout> const TypeLayout for core::option::Option<T>
where
    [u8; core::mem::size_of::<core::mem::Discriminant<Self>>()]:,
{
    const TYPE_LAYOUT: TypeLayoutInfo<'static> = TypeLayoutInfo {
        name: ::core::any::type_name::<Self>(),
        size: ::core::mem::size_of::<Self>(),
        alignment: ::core::mem::align_of::<Self>(),
        structure: TypeStructure::Enum {
            repr: "",
            variants: &[
                Variant {
                    name: "None",
                    discriminant: crate::struct_variant_discriminant!(
                        Option => Option<T> => None
                    ),
                    fields: &[],
                },
                Variant {
                    name: "Some",
                    discriminant: crate::struct_variant_discriminant!(
                        Option => Option<T> => Some(f_0: T)
                    ),
                    fields: &[Field {
                        name: "0",
                        offset: crate::struct_variant_field_offset!(Option => Option<T> => Some(f_0: T) => 0),
                        ty: ::core::any::type_name::<T>(),
                    }],
                },
            ],
        },
    };

    unsafe fn uninit() -> MaybeUninhabited<core::mem::MaybeUninit<Self>> {
        MaybeUninhabited::Inhabited(core::mem::MaybeUninit::new(None))
    }
}

unsafe impl<T: ~const TypeGraph + ~const TypeLayout> const TypeGraph for core::option::Option<T>
where
    [u8; core::mem::size_of::<core::mem::Discriminant<Self>>()]:,
{
    fn populate_graph(graph: &mut TypeLayoutGraph<'static>) {
        if graph.insert(&Self::TYPE_LAYOUT) {
            <T as TypeGraph>::populate_graph(graph);
        }
    }
}

unsafe impl<T: ComputeTypeSet> ComputeTypeSet for core::option::Option<T> {
    type Output<R: ExpandTypeSet> = Set<Self, tset![T, .. @ R]>;
}
