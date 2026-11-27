//! Turning mesh streams into a solid, and a solid back into mesh streams.

use manifold_rust::manifold::Manifold;
use manifold_rust::types::MeshGL;

use crate::lit::OwnedLitMeshDesc;
use crate::manifold::{Error, channel_of};

/// The property count an exported solid states when it holds positions alone.
const POSITIONS_ONLY: usize = 3;
/// The property count an exported solid states when it holds positions and then
/// normals, which is the layout [`Manifold::calculate_normals`] writes at slot
/// zero.
const POSITIONS_AND_NORMALS: usize = 6;

/// Build a solid from a closed, consistently wound triangle mesh.
///
/// The mesh is taken as the solid it describes: a tunnel, a bracket or any
/// other shape whose surface closes on itself, with every triangle wound
/// counter-clockwise seen from outside. A mesh that fails either condition is
/// refused — the kernel states
/// [`Error::Rejected`] around a diagnosis of
/// `NotManifold` or `NotClosed` — because no boolean over it would be
/// watertight.
///
/// Only [`positions`](OwnedLitMeshDesc::positions) and
/// [`indices`](OwnedLitMeshDesc::indices) are read, and both move rather than
/// copy: the positions become the kernel's own vertex array. See the [module
/// documentation](crate::manifold) for why the other channels are refused.
///
/// A mesh whose indices were left empty names its triangles by position, so its
/// vertex count must be a multiple of three.
///
/// Normals are not needed and not read. The kernel infers each face's plane
/// from its vertices, which is all the booleans use.
///
/// Triangles that name their corners by position — a mesh with no indices and
/// three vertices per triangle — state each shared corner once per triangle,
/// and the strict import reads that as a surface that does not close. Such a
/// mesh takes [`from_mesh_robust`], which welds the corners first.
///
/// # Errors
///
/// [`Error::EmptyInput`] for a mesh with no vertices,
/// [`Error::UnsupportedChannel`] for a channel other than positions and
/// indices, [`Error::RaggedIndices`] for a non-indexed mesh that is not a whole
/// number of triangles, and [`Error::Rejected`] when the kernel will not accept
/// the geometry as a solid.
pub fn from_mesh(mesh: OwnedLitMeshDesc) -> Result<Manifold, Error> {
    import(mesh, Manifold::from_mesh_gl)
}

/// Build a solid from a triangle mesh that need not be closed, in the kernel's
/// robust mode.
///
/// [`from_mesh`] demands a watertight, consistently wound surface, because that
/// is what makes a boolean over it exact. Geometry that arrives as a triangle
/// soup — a mesh converted from another format, or one whose seams a generator
/// did not weld — takes this entry instead: the kernel repairs the surface by
/// welding coincident vertices before it builds the topology.
///
/// The repaired solid supports the robust boolean engine, transformation and
/// export, and refuses every operation that needs a paired topology — including
/// [`Manifold::calculate_normals`], which answers an empty solid rather than
/// normals. Prefer [`from_mesh`] when the geometry is already closed (as the
/// kernel's own constructors and booleans produce), and reach for this one for
/// geometry the caller did not build.
///
/// # Errors
///
/// The same refusals [`from_mesh`] reports; a soup the kernel cannot repair is
/// [`Error::Rejected`] around `NotClosed`.
pub fn from_mesh_robust(mesh: OwnedLitMeshDesc) -> Result<Manifold, Error> {
    import(mesh, Manifold::from_mesh_gl_robust)
}

/// Read a solid back as the vertex streams a lit mesh is uploaded from.
///
/// A solid that carries nothing but positions comes back with
/// [`normals`](OwnedLitMeshDesc::normals) empty, which is the case worth
/// choosing: the lit pipeline then infers one geometric normal per triangle
/// from screen-space derivatives, which is a hard edge at every face without a
/// single extra vertex. A solid that went through
/// [`Manifold::calculate_normals`] at slot zero comes back with those normals.
///
/// The result is an indexed triangle mesh wound counter-clockwise from outside,
/// which is the winding back-face culling expects.
///
/// # Errors
///
/// [`Error::EmptyResult`] for a solid with no triangles — a boolean that found
/// no overlap answers one, and a pooled mesh covers at least one vertex, so the
/// upload could not take it. [`Error::NonFiniteVertex`] when a coordinate is
/// not finite. [`Error::UnknownChannels`] when the solid carries an extra
/// property layout other than none or three normal components, which this
/// module cannot tell apart from normals.
pub fn streams(solid: &Manifold) -> Result<OwnedLitMeshDesc, Error> {
    if solid.is_empty() {
        return Err(Error::EmptyResult);
    }
    // A negative index asks for whichever slot `calculate_normals` filled, so a
    // solid whose normals were computed at slot zero exports them without the
    // caller having to remember which slot that was. Without them the export
    // holds positions alone.
    let mesh: MeshGL = solid.get_mesh_gl(-1);
    let stride = mesh.num_prop as usize;
    let positions = stride_of(&mesh, 0);
    let normals = match stride {
        POSITIONS_ONLY => None,
        POSITIONS_AND_NORMALS => Some(stride_of(&mesh, POSITIONS_ONLY)),
        num_prop => return Err(Error::UnknownChannels { num_prop }),
    };
    if positions
        .iter()
        .chain(normals.iter().flatten())
        .flatten()
        .any(|value| !value.is_finite())
    {
        return Err(Error::NonFiniteVertex);
    }
    Ok(OwnedLitMeshDesc {
        positions,
        normals,
        indices: Some(mesh.tri_verts),
        ..Default::default()
    })
}

/// Copy one channel out of the interleaved array an export states.
///
/// An export holds one row per vertex and property together, `num_prop` values
/// to a row; a mesh upload wants each channel flat, so a channel is the columns
/// starting at `offset` rather than a contiguous run.
fn stride_of(mesh: &MeshGL, offset: usize) -> Vec<[f32; 3]> {
    let stride = mesh.num_prop as usize;
    mesh.vert_properties
        .chunks_exact(stride)
        .map(|row| [row[offset], row[offset + 1], row[offset + 2]])
        .collect()
}

/// Check a description, move it into the kernel's `MeshGL`, and let the kernel
/// import it.
fn import(mesh: OwnedLitMeshDesc, build: fn(&MeshGL) -> Manifold) -> Result<Manifold, Error> {
    if let Some(channel) = channel_of(&mesh) {
        return Err(Error::UnsupportedChannel { channel });
    }
    let vertices = mesh.positions.len();
    if vertices == 0 {
        return Err(Error::EmptyInput);
    }
    let indices = match mesh.indices {
        Some(indices) => indices,
        None if vertices.is_multiple_of(3) => (0..vertices as u32).collect(),
        None => return Err(Error::RaggedIndices { count: vertices }),
    };
    // The kernel states positions as one flat array of interleaved triples,
    // which is the layout `Vec<[f32; 3]>` already has: flattening it moves the
    // same allocation rather than copying the values.
    let mesh_gl = MeshGL {
        num_prop: POSITIONS_ONLY as u32,
        vert_properties: mesh.positions.into_flattened(),
        tri_verts: indices,
        // Zero leaves the tolerance to the kernel, which derives it from the
        // float precision and the bounding box.
        ..Default::default()
    };
    let solid = build(&mesh_gl);
    match solid.status() {
        manifold_rust::types::Error::NoError if solid.is_empty() => Err(Error::EmptyInput),
        manifold_rust::types::Error::NoError => Ok(solid),
        status => Err(Error::Rejected { status }),
    }
}
