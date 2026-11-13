# Workarounds

Driver and backend bugs this repository has to route around, why the obvious
code is wrong, and what the workaround costs. Each entry names the place the
workaround lives, so a later reader can find it without searching for the
symptom first.

The code that works around one carries a comment pointing here.

English | [简体中文](WORKAROUND.zh-CN.md)

## A dynamic subscript of a vector reads zero lanes on DX12

**Where:** [`crates/unlit_wgpu/shaders/array_access.wesl`](crates/unlit_wgpu/shaders/array_access.wesl)
`load_cell_word`, the texel path taken when the device has no storage buffers
(WebGL2).

**Symptom:** on the WebGL2 tier over DX12, the lit scenes lost every point and
polygon light and kept only spot lights. The cluster cell a fragment read was
the right one and its light count was right, but every light index inside the
cell came back as 0, so all of them resolved to the light at index 0 — which in
the example scene is a spot light. Nothing warned: the frame simply drew wrong.

**Cause:** the cell record is stored four `u32` words to an `Rgba32Float` texel,
and unpacking one word means picking a lane of that texel:

```wgsl
let texel = textureLoad(light_cells_tex, texel_of(light_cells_tex, first), 0);
return bitcast<u32>(texel[word % 4u]); // every lane reads 0 on DX12
```

WGSL allows this and naga translates it faithfully (`v[min(uint(lane), 3u)]` in
HLSL). FXC accepts it — it is a legal r-value subscript, so there is no
`X3500` — and then emits DXBC that reads lane 0 whatever the index is. naga
already knows FXC miscompiles vector subscripts behind a value pointer
([naga#2095](https://github.com/gfx-rs/naga/issues/2095), fixed for *constant*
subscripts by [naga#2096](https://github.com/gfx-rs/naga/pull/2096)), but a
subscript by a variable still goes through
([wgpu#4460](https://github.com/gfx-rs/wgpu/issues/4460) covers the l-value
form only).

A subscript of an *array* is fine: naga spills the value to a temporary array
and FXC addresses it natively
([wgpu#4337](https://github.com/gfx-rs/wgpu/issues/4337)). A vector lane has no
such addressing, which is why the array fix does not extend to it.

**Workaround:** name the lanes and select the one asked for, so no dynamic
subscript reaches the compiler:

```wgsl
let lane = word % 4u;
let lanes = vec4<u32>(bitcast<u32>(texel.x), bitcast<u32>(texel.y),
                      bitcast<u32>(texel.z), bitcast<u32>(texel.w));
return lanes.x * select(0u, 1u, lane == 0u)
     + lanes.y * select(0u, 1u, lane == 1u)
     + lanes.z * select(0u, 1u, lane == 2u)
     + lanes.w * select(0u, 1u, lane == 3u);
```

**Cost:** four multiplies, three adds and four selects per word instead of one
indexed read, in the light loop of every lit fragment. The alternative —
unpacking the cell into a `var` array first — trades the same work for a
temporary array and is what the WebGPU path's storage buffer already gives us.

**Rule:** every texel unpacking in a shader picks its lane by name. There is no
dynamic vector subscript left in the shader tree; keep it that way.

## A clipped draw left last clips the pass's multisample resolve on GL

**Where:** [`crates/unlit3d/src/renderer.rs`](crates/unlit3d/src/renderer.rs),
the end of `record_pass`, after every source has recorded.

**Symptom:** with MSAA on the GL backend, a scene whose last draw was clipped —
a UI window, say — resolved only that rectangle and left the rest of the frame
holding the previous frame's pixels.

**Cause:** the scissor rectangle is pass state, and unlike the viewport wgpu
gives no call to reset it. On the GL backend the multisample resolve that ends
a pass is a blit, and a blit is clipped by the scissor in effect when the pass
ends. So the clip of the last draw decides how much of the frame is resolved.

**Workaround:** cover the whole target once all sources have recorded:

```rust
render_pass.set_scissor_rect(0, 0, attachments.width(), attachments.height());
```

This also lets [`DrawEntry::scissor`](crates/unlit_wgpu/src/scene.rs) mean what
it says — `None` leaves whatever the pass already has rather than implying the
whole target — because the pass no longer depends on any draw having set one.

**Cost:** one redundant state set per pass.

**Rule:** a pass that draws anything clipped covers the whole target before it
ends. The regression test is `a_clipped_panel_does_not_clip_the_resolve` in
[`crates/unlit3d/tests/gpu_ui_source.rs`](crates/unlit3d/tests/gpu_ui_source.rs).

## A derivative inside a per-fragment branch fails to compile on WebGPU

**Where:** [`crates/unlit_wgpu/shaders/lit_lighting.wesl`](crates/unlit_wgpu/shaders/lit_lighting.wesl)
`fragment_normal`, which takes both derivatives before the branch that follows
them.

**Symptom:** every lit test fails on the browser's WebGPU backend, and only
there. The page logs

```text
Error while parsing WGSL: :162:43 error: 'dpdy' must only be called from uniform control flow
```

followed by an invalid `ShaderModule`, an invalid `RenderPipeline` and an
invalid `CommandBuffer`, so the frame draws nothing and the assertion fails on a
canvas of zeroes.

**Cause:** WGSL requires a derivative built-in — `dpdx`, `dpdy`, `fwidth` — to be
called from uniform control flow. The shader decided the normal first and only
then asked for the geometric normal:

```wgsl
if length_squared > 1.0e-8 { return in.normal * inverseSqrt(length_squared); }
var geometric = cross(dpdx(in.world), dpdy(in.world)); // non-uniform control flow
```

`length_squared` is interpolated per fragment, so the branch is non-uniform and
so is everything after it. Only Tint, the validator behind the browser's WebGPU,
runs the uniformity analysis; naga does not, so no host backend ever rejected the
module and the mistake was invisible until a browser compiled it. That gap is
tracked in [wgpu#4369](https://github.com/gfx-rs/wgpu/issues/4369): naga's
fragment-stage uniformity requirement is compiled out by a
`DISABLE_UNIFORMITY_REQ_FOR_FRAGMENT_STAGE` const in `naga/src/valid/analyzer.rs`,
which zeroes the `DERIVATIVE` and `IMPLICIT_LEVEL` bits, so derivatives are never
checked for uniformity on the host. The branch is behind `@if(VERTEX_NORMAL)`, so
only the shaders that carry vertex normals are affected.

**Workaround:** take the derivatives first, and branch afterwards:

```wgsl
let world_dx = dpdx(in.world);
let world_dy = dpdy(in.world);
@if(VERTEX_NORMAL) { … }
var geometric = cross(world_dx, world_dy);
```

This is also the shape the spec asks for, so there is nothing left to detect:
the module is valid on every backend.

**Cost:** two derivative readings per lit fragment are taken even when the vertex
normal decides the answer and they are thrown away. A derivative is a quad-wide
subtract on values already in registers, so this costs less than the branch it
moved above.

**Rule:** a derivative built-in is called before anything in the function
branches, or not at all. There are only these two calls in the shader tree; keep
it that way. Nothing tests this: the suites compose every variant and validate
it with naga, which is the validator that never asks the question. Only a
browser's WebGPU backend compiles a shader with Tint, and only the lit tests get
far enough for one to be told apart from a device that went away.
