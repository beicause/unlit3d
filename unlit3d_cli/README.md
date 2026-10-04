English | [简体中文](https://github.com/beicause/unlit3d/blob/main/unlit3d_cli/README.zh-CN.md)

# unlit3d_cli

A command-line tool that renders glTF documents to an image. The package is
`unlit3d_cli` and the binary it builds is `unlit3d-cli`. It is at an **early
stage** along with the crates it uses.

## What it does

It loads one or more glTF documents, places each one, samples an animation at a
given time, draws the scene through a camera, and writes the frame to a file.
Every part of that is configurable: a JSON configuration file describes the
whole render, and command-line options override the same-named fields of it.

The command line is parsed with [`argh`](https://docs.rs/argh), so every option
is `--name value` or a bare flag, and the value must be the next argument;
`--name=value` is not accepted. `--output` is the only required option, and
its extension chooses the image format: `png`, `webp` and `jpeg` are supported.

```text
cargo run -p unlit3d_cli --bin unlit3d-cli -- --output frame.png --document unlit3d_asset_files/assets/Fox.glb --animation Walk --time 0.2
```

## Output and render size

A render has two sizes. The **output size** is the image that is written. The
**render size** is what the camera and the viewport are based on: it sets the
camera's aspect ratio and the shape of the region the scene is drawn into. The
viewport is the render size scaled by `scale` and centred in the output.
`scale` is a pair `[scale_x, scale_y]`, so the two axes scale independently:
a 512x384 output with a 256x192 render size and `scale [2.0, 2.0]` draws the
scene at double size and fills the frame, while a larger output with the same
render size and `scale [1.0, 1.0]` letterboxes it.

There is one texture, at the output size; the scene is drawn straight into it,
so the picture is never resampled. `render_size` defaults to the output size,
and the output size defaults to the render size, so at least one of the two must
be given.

## Camera

A camera is either built from `eye`, `target`, `up`, `fov_y` (degrees) and
`z_near`, or set outright as a column-major 4x4 `matrix`. The matrix wins when
both are given: it is the full clip-from-world transform, while the named fields
build a right-handed look-at view and a DirectX-style infinite-reverse
projection, the same camera `unlit3d_examples` draws its scenes with.

## Configuration file

`--config` reads a JSON file before the overrides are applied. Every field is
optional and unknown fields are refused. The shape is:

```json
{
  "output": {
    "size": [512, 384],
    "render_size": [256, 192],
    "scale": [2.0, 2.0],
    "samples": 1,
    "depth": true,
    "clear": [0.0, 0.0, 0.0, 1.0]
  },
  "camera": {
    "eye": [160.5, 109.0, 49.5],
    "target": [4.5, 38.0, 26.5],
    "up": [0.0, 1.0, 0.0],
    "fov_y": 60.0,
    "z_near": 0.1
  },
  "documents": [
    {
      "path": "unlit3d_asset_files/assets/Fox.glb",
      "placement": {
        "translation": [-4.54, 0.0, -1.01],
        "rotation": [0.0, -0.21643962, 0.0, 0.976296],
        "scale": [1.0, 1.0, 1.0]
      },
      "animation": "Walk",
      "time": 0.0
    }
  ]
}
```

`animation` names one of the document's animations, either by index or by name;
`time` is the animation time in seconds. A document is drawn with its own scene,
and every node it spawns takes the `placement` transform when one is given.

On the command line, `--document` is repeatable and replaces the configuration's
document list, while `--animation` and `--time` apply to every document.

## Library

The binary only parses the arguments and writes the file. The render itself is
the library's `render` function, which builds a headless GPU device, draws the
scene into an offscreen target and reads the pixels back, and `save`, which
encodes a frame to a path. Both are what the end-to-end test drives.

## Tests

`tests/gpu_cli.rs` is the end-to-end check. It renders the reference glTF scene
of `unlit3d_examples/src/scenes/gltf.rs` through the library at each of the six
animation phases and compares every frame against the stored snapshots in
`unlit3d_asset_files/snapshots/gltf/`. It also checks a narrow render size — a
192x288 output with a 256x192 render size scaled by `288/192` on both axes —
against a snapshot of its own, and that `save` writes png, webp and jpeg.

```text
cargo nextest run -p unlit3d_cli
```

The snapshot comparison uses SSIMULACRA2. A missing snapshot is written from the
frame rather than failed on, and `SNAPSHOT_UPDATE=1` rewrites one that exists.

## License

Dual-licensed under MIT or Apache-2.0, at your option.
