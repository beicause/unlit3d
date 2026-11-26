use super::ids::{AnimationId, ImageId, MaterialId, MeshId, NodeId, PrimitiveId};

/// Everything that can stop a load, a spawn or an animation of a glTF document.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The document could not be read, validated or decoded — the glTF crate's
    /// own report, which covers the file's JSON, its buffers and its images.
    #[error(transparent)]
    Parse(#[from] gltf::Error),
    /// A caller named a position its document does not carry.
    ///
    /// Typed ids are positions, not proofs: `ImageId::new(7)` names the eighth
    /// image of whatever document it is read against, and a document with fewer
    /// images reports this instead of reading past its end.
    #[error("the document carries no image {image}")]
    ImageOutsideDocument {
        /// The image the caller named.
        image: ImageId,
    },
    /// A caller named an animation its document does not carry.
    #[error("the document carries no animation {animation}")]
    AnimationOutsideDocument {
        /// The animation the caller named.
        animation: AnimationId,
    },
    /// A caller named a node its document does not carry.
    #[error("the document carries no node {node}")]
    NodeOutsideDocument {
        /// The node the caller named.
        node: NodeId,
    },
    /// A caller named a material its document does not carry.
    #[error("the document carries no material {material}")]
    MaterialOutsideDocument {
        /// The material the caller named.
        material: MaterialId,
    },
    /// A caller named a primitive its document does not carry.
    #[error("the document carries no primitive {primitive} of mesh {mesh}")]
    MeshOutsideDocument {
        /// The mesh the caller named.
        mesh: MeshId,
        /// The primitive of that mesh the caller named.
        primitive: PrimitiveId,
    },
    /// A primitive's mesh was never inserted into the
    /// [`super::GltfResources`] the spawn draws from.
    #[error("the mesh of node {node} was not inserted into the resources")]
    MeshNotInserted {
        /// The node that draws the missing mesh.
        node: NodeId,
    },
    /// A primitive samples its material's base-color texture, but no material
    /// was inserted for its document material.
    ///
    /// The resources carry no entry for the material at all, which is what
    /// [`Self::MaterialWithoutTexture`] distinguishes from an entry that was
    /// uploaded without a base-color texture.
    #[error(
        "the primitive {primitive} of mesh {mesh} samples a base-color texture, \
         but no material was inserted for it"
    )]
    MaterialNotInserted {
        /// The mesh the primitive belongs to.
        mesh: MeshId,
        /// The primitive that samples the texture.
        primitive: PrimitiveId,
    },
    /// A primitive samples its material's base-color texture, but the material
    /// was uploaded without one.
    ///
    /// A material that names no base-color texture is uploaded as `None` in the
    /// resources, so a caller whose resources disagree with the document about
    /// which materials are textured lands here rather than drawing the
    /// primitive untextured.
    #[error(
        "the primitive {primitive} of mesh {mesh} samples a base-color texture, \
         but its material was inserted without one"
    )]
    MaterialWithoutTexture {
        /// The mesh the primitive belongs to.
        mesh: MeshId,
        /// The primitive that samples the texture.
        primitive: PrimitiveId,
    },
    /// The document declares no default scene, so [`super::LitGltf::to_model`] has no
    /// root nodes to instantiate.
    #[error("the glTF document declares no default scene")]
    NoDefaultScene,
    /// A skin's joint was left out of the node set the model carries.
    ///
    /// A joint and its whole ancestry join the model even when the default
    /// scene leaves them outside, so this reports a document whose joint list
    /// disagrees with the node list it is read against.
    #[error("joint {joint} of the skin of node {node} is not a node of the model")]
    JointOutsideModel {
        /// The skinned node.
        node: NodeId,
        /// The joint index inside that skin.
        joint: usize,
    },
    /// The document decoded into model data the model refused.
    ///
    /// The document is the source of the inconsistency, not the model: a skin
    /// whose joints fall outside the nodes the default scene reaches, or an
    /// accessor that states fewer values than its key times name, lands here.
    #[error(transparent)]
    Model(#[from] crate::lit::LitModelError),
    /// A material samples its base-color texture, but the image the texture
    /// names was not among the images the caller inserted.
    ///
    /// A caller that inserts a document piecewise has to insert the images
    /// before the materials that name them; one that calls
    /// [`super::LitGltf::insert_material`] without the image lands here.
    #[error(
        "material {material} samples the image {image}, \
         which was not among the images it was inserted with"
    )]
    ImageNotInserted {
        /// The document material that samples the missing image.
        material: MaterialId,
        /// The document image the material names.
        image: ImageId,
    },
    /// One mesh declares different morph target counts across its primitives.
    ///
    /// A weights channel slices one value per target per keyframe, so the count
    /// has to be the same for every primitive of a mesh. The specification
    /// requires it; a document that disagrees with itself cannot be read
    /// consistently and is refused rather than resolved to one of the counts.
    #[error(
        "the primitive {primitive} of mesh {mesh} declares {targets} morph targets, \
         but the mesh's first primitive declares {expected}"
    )]
    MorphTargetCountMismatch {
        /// The mesh whose primitives disagree.
        mesh: MeshId,
        /// The primitive that declares the other count.
        primitive: PrimitiveId,
        /// The number of targets that primitive declares.
        targets: usize,
        /// The number of targets the mesh's first primitive declares.
        expected: usize,
    },
    /// A line loop or a triangle fan holds too few vertices to draw one
    /// primitive, so its index stream cannot be expanded.
    #[error(
        "the primitive {primitive} of mesh {mesh} is a {mode} of {vertices} vertices, \
         which is too short to draw"
    )]
    PrimitiveTooShort {
        /// The mesh the primitive belongs to.
        mesh: MeshId,
        /// The primitive that is too short.
        primitive: PrimitiveId,
        /// The glTF mode name the primitive declares.
        mode: &'static str,
        /// The number of vertices in the primitive's index stream.
        vertices: usize,
    },
    /// A primitive's index stream names a vertex its vertex streams do not
    /// carry.
    #[error(
        "the primitive {primitive} of mesh {mesh} indexes vertex {index}, \
         but the primitive carries {vertices} vertices"
    )]
    IndexOutOfRange {
        /// The mesh the primitive belongs to.
        mesh: MeshId,
        /// The primitive whose index stream overruns its vertices.
        primitive: PrimitiveId,
        /// The first index past the end of the vertex streams.
        index: u32,
        /// The number of vertices the primitive carries.
        vertices: usize,
    },
    /// An image's pixel buffer is too short for the dimensions it states.
    #[error(
        "image {image} states {width} by {height} pixels of {texel} bytes, \
         which needs {expected} bytes of pixels, but it carries {actual}"
    )]
    ImagePixelsShort {
        /// The image whose pixel buffer is short.
        image: ImageId,
        /// The width the image states.
        width: u32,
        /// The height the image states.
        height: u32,
        /// The bytes one texel of the image's format occupies.
        texel: usize,
        /// The bytes the stated dimensions need.
        expected: usize,
        /// The bytes the image actually carries.
        actual: usize,
    },
    /// A decoded Draco stream could not be turned back into accessors.
    ///
    /// The document is malformed in a way the extension's own reader reports,
    /// so the Draco error itself is carried: its kind and detail stay readable
    /// rather than being restated as a glTF validation failure with no context.
    #[error(
        "the Draco stream of the primitive {primitive} of mesh {mesh} cannot be read: {source}"
    )]
    Draco {
        /// The mesh the compressed primitive belongs to.
        mesh: MeshId,
        /// The compressed primitive.
        primitive: PrimitiveId,
        /// The Draco reader's own report.
        #[source]
        source: draco_core::DracoError,
    },
    /// A Draco primitive states something the extension cannot express.
    ///
    /// The extension compresses a triangle primitive's attributes and indices
    /// only, so a document that asks for anything else is refused rather than
    /// silently decoded into the nearest supported shape.
    #[error("the Draco primitive {primitive} of mesh {mesh}: {detail}")]
    DracoPrimitive {
        /// The mesh the compressed primitive belongs to.
        mesh: MeshId,
        /// The compressed primitive.
        primitive: PrimitiveId,
        /// What the extension cannot express about it.
        detail: DracoPrimitiveError,
    },
    /// A primitive carries no vertex positions.
    ///
    /// glTF validation rejects such a primitive, so a document that reaches this
    /// error came through a path that skipped validation.
    #[error("the primitive {primitive} of mesh {mesh} carries no POSITION attribute")]
    PositionsMissing {
        /// The mesh the primitive belongs to.
        mesh: MeshId,
        /// The primitive without positions.
        primitive: PrimitiveId,
    },
}

#[cfg(all(feature = "mcp", not(target_arch = "wasm32")))]
impl Error {
    /// The failure as structured content: the variant's name under `kind`, and
    /// the document positions that disagree under the names the variant gives
    /// them.
    ///
    /// A caller of a tool that loads a document branches on this rather than on
    /// the message: `kind` says which check failed, and the numeric fields name
    /// the mesh, primitive or image to correct. A cause that is another error
    /// type — a parse failure, a model the data cannot express — carries its
    /// own message under `message`.
    #[must_use]
    pub fn to_value(&self) -> unlit_ecs::mcp::serde_json::Value {
        use unlit_ecs::mcp::serde_json::json;
        match self {
            Self::Parse(error) => json!({ "kind": "Parse", "message": error.to_string() }),
            Self::ImageOutsideDocument { image } => {
                json!({ "kind": "ImageOutsideDocument", "image": image.index() })
            }
            Self::AnimationOutsideDocument { animation } => json!({
                "kind": "AnimationOutsideDocument",
                "animation": animation.index(),
            }),
            Self::NodeOutsideDocument { node } => {
                json!({ "kind": "NodeOutsideDocument", "node": node.index() })
            }
            Self::MaterialOutsideDocument { material } => json!({
                "kind": "MaterialOutsideDocument",
                "material": material.index(),
            }),
            Self::MeshOutsideDocument { mesh, primitive } => json!({
                "kind": "MeshOutsideDocument",
                "mesh": mesh.index(),
                "primitive": primitive.index(),
            }),
            Self::MeshNotInserted { node } => {
                json!({ "kind": "MeshNotInserted", "node": node.index() })
            }
            Self::MaterialNotInserted { mesh, primitive } => json!({
                "kind": "MaterialNotInserted",
                "mesh": mesh.index(),
                "primitive": primitive.index(),
            }),
            Self::MaterialWithoutTexture { mesh, primitive } => json!({
                "kind": "MaterialWithoutTexture",
                "mesh": mesh.index(),
                "primitive": primitive.index(),
            }),
            Self::NoDefaultScene => json!({ "kind": "NoDefaultScene" }),
            Self::JointOutsideModel { node, joint } => json!({
                "kind": "JointOutsideModel",
                "node": node.index(),
                "joint": joint,
            }),
            Self::Model(error) => json!({ "kind": "Model", "message": error.to_string() }),
            Self::ImageNotInserted { material, image } => json!({
                "kind": "ImageNotInserted",
                "material": material.index(),
                "image": image.index(),
            }),
            Self::MorphTargetCountMismatch {
                mesh,
                primitive,
                targets,
                expected,
            } => json!({
                "kind": "MorphTargetCountMismatch",
                "mesh": mesh.index(),
                "primitive": primitive.index(),
                "targets": targets,
                "expected": expected,
            }),
            Self::PrimitiveTooShort {
                mesh,
                primitive,
                mode,
                vertices,
            } => json!({
                "kind": "PrimitiveTooShort",
                "mesh": mesh.index(),
                "primitive": primitive.index(),
                "mode": mode,
                "vertices": vertices,
            }),
            Self::IndexOutOfRange {
                mesh,
                primitive,
                index,
                vertices,
            } => json!({
                "kind": "IndexOutOfRange",
                "mesh": mesh.index(),
                "primitive": primitive.index(),
                "index": index,
                "vertices": vertices,
            }),
            Self::ImagePixelsShort {
                image,
                width,
                height,
                texel,
                expected,
                actual,
            } => json!({
                "kind": "ImagePixelsShort",
                "image": image.index(),
                "width": width,
                "height": height,
                "texel": texel,
                "expected": expected,
                "actual": actual,
            }),
            Self::Draco {
                mesh,
                primitive,
                source,
            } => json!({
                "kind": "Draco",
                "mesh": mesh.index(),
                "primitive": primitive.index(),
                "message": source.to_string(),
            }),
            Self::DracoPrimitive {
                mesh,
                primitive,
                detail,
            } => json!({
                "kind": "DracoPrimitive",
                "mesh": mesh.index(),
                "primitive": primitive.index(),
                "detail": detail.to_string(),
            }),
            Self::PositionsMissing { mesh, primitive } => json!({
                "kind": "PositionsMissing",
                "mesh": mesh.index(),
                "primitive": primitive.index(),
            }),
        }
    }
}

/// What a Draco primitive states that the extension cannot express.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum DracoPrimitiveError {
    /// The primitive's mode is neither a triangle list nor a triangle strip.
    #[error("its mode is not a triangle primitive")]
    NotTriangles,
    /// Its extension names no buffer view to decode.
    #[error("its extension names no buffer view")]
    ExtensionNamesNoStream,
    /// Its extension maps no semantic to a stream attribute.
    #[error("its extension maps no semantic to a stream attribute")]
    ExtensionMapsNoAttributes,
    /// The buffer view the extension names is absent from the document.
    #[error("its extension names buffer view {view}, which is missing")]
    MissingStream {
        /// The buffer view the extension names.
        view: usize,
    },
    /// The buffer view the extension names reads a buffer the document lacks.
    #[error("buffer view {view} reads buffer {buffer}, which is missing")]
    MissingStreamBuffer {
        /// The buffer view the extension names.
        view: usize,
        /// The buffer that view reads.
        buffer: usize,
    },
    /// The stream's byte range overflows the address space.
    #[error("the byte range of buffer view {view} overflows")]
    StreamOverflowsBuffer {
        /// The buffer view the extension names.
        view: usize,
    },
    /// The stream's byte range extends past the buffer that holds it.
    #[error("buffer view {view} extends past the buffer it reads")]
    StreamPastBuffer {
        /// The buffer view the extension names.
        view: usize,
    },
    /// The extension compresses a semantic the primitive does not declare.
    #[error("its stream supplies {semantic}, which the primitive does not declare")]
    AttributeNotDeclared {
        /// The semantic the extension supplies.
        semantic: String,
    },
    /// An attribute id the extension names does not fit the stream's id space.
    #[error("the attribute id {id} is too large")]
    AttributeIdTooLarge {
        /// The attribute id the extension names.
        id: u64,
    },
    /// A morph target reads an accessor the extension also compresses, which
    /// would leave the target with no data of its own.
    #[error("morph target {target} names the accessor {accessor}, which is in the stream")]
    MorphTargetInStream {
        /// The morph target's position in the primitive's target list.
        target: usize,
        /// The accessor the target names.
        accessor: usize,
    },
    /// Its accessor states a component type no accessor type names.
    #[error("its accessor states no type the document can name")]
    InvalidAccessorType,
    /// Its accessor is missing from the document.
    #[error("its accessor {accessor} is missing from the document")]
    MissingAccessor {
        /// The accessor id the primitive names.
        accessor: usize,
    },
    /// The stream carries fewer components than its accessor declares.
    #[error("it carries fewer components than its accessor declares")]
    TooFewComponents,
    /// An attribute id it names is absent from the stream.
    #[error("the attribute id {id} is absent from the stream")]
    MissingAttribute {
        /// The attribute id the extension maps a semantic to.
        id: u32,
    },
    /// A joint index does not fit the 16-bit joint stream.
    #[error("it names joint {joint}, which the 16-bit joint stream cannot hold")]
    JointOutOfRange {
        /// The joint index the stream states.
        joint: f32,
    },
    /// A normalized attribute's integer type cannot be normalized without
    /// losing precision in the 32-bit float the accessor holds.
    #[error("its {data_type} integers cannot be normalized without losing precision")]
    NormalizationTooWide {
        /// The component type the attribute holds.
        data_type: &'static str,
    },
}
