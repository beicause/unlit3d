//! The ids a glTF document's collections are addressed by.

/// An index into one of a glTF document's collections.
///
/// Each kind of thing a document holds has its own id type, so a node index
/// cannot be passed where an animation or an image is expected. An id is a
/// zero-based position; `LitGltf`'s iterator methods yield the ids a
/// document actually carries.
macro_rules! document_index {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(usize);

        impl $name {
            /// The id at a zero-based position in the document.
            ///
            /// Nothing checks the position here, because an id names a document
            /// and this constructor has none: it is for the ids a document's own
            /// iterators and lookups hand out, and for naming a position in test
            /// data. Lookups on [`super::LitGltf`] check the id against
            /// the document's own collection.
            #[must_use]
            pub const fn new(index: usize) -> Self {
                Self(index)
            }

            /// The zero-based position in the document.
            #[must_use]
            pub const fn index(self) -> usize {
                self.0
            }
        }

        impl core::fmt::Display for $name {
            fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                core::fmt::Display::fmt(&self.0, formatter)
            }
        }
    };
}

document_index!(
    /// An index into a document's images.
    ImageId
);
document_index!(
    /// An index into a document's materials.
    MaterialId
);
document_index!(
    /// An index into a document's meshes.
    MeshId
);
document_index!(
    /// An index into one mesh's primitives.
    PrimitiveId
);
document_index!(
    /// An index into a document's nodes.
    NodeId
);
document_index!(
    /// An index into a document's animations.
    AnimationId
);
