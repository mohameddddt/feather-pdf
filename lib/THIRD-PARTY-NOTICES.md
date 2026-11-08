# Third-party notices

Feather PDF's own code is MIT-licensed (see [LICENSE](../LICENSE)). It ships with
`pdfium.dll`, which is built from third-party code under its own licenses.

## PDFium

`pdfium.dll` is PDFium (https://pdfium.googlesource.com/pdfium/), build 159.0.8149,
compiled by the [pypdfium2](https://github.com/pypdfium2-team/pypdfium2) project
(taken from the `pypdfium2-5.15.1-py3-none-win_amd64` wheel on PyPI, unmodified).
PDFium is licensed under the BSD 3-Clause license and Apache 2.0; it includes
third-party libraries under their own licenses.

The full license texts are in [licenses/](licenses/):

| Component | License file |
|---|---|
| PDFium | `pdfium.txt` |
| pdfium-binaries build scripts | `pdfium-binaries.txt` |
| Abseil, Highway | `abseil.txt`, `highway.txt`, `SHARED/Apache-2.0.txt` |
| Anti-Grain Geometry 2.3 | `agg23.txt` |
| Brotli | `pbin_and_sbuild/brotli.txt` |
| Dragonbox | `dragonbox-Apache2-LLVM.txt`, `dragonbox-Boost.txt` |
| fast_float | `fast_float.txt` |
| FreeType | `freetype.txt` |
| HarfBuzz | `harfbuzz.txt` |
| ICU | `icu.txt` |
| Little CMS | `lcms.txt` |
| libjpeg-turbo | `libjpeg_turbo.ijg`, `libjpeg_turbo.md` |
| libyuv | `pbin_and_sbuild/libyuv.txt` |
| LLVM libc | `all_except_musl/llvm-libc.txt` |
| OpenJPEG | `libopenjpeg.txt` |
| libpng | `libpng.txt` |
| LibTIFF | `libtiff.txt` |
| simdutf | `simdutf.txt` |
| zlib | `zlib.txt` |

## Windows icon fonts

The toolbar icons are drawn with Segoe Fluent Icons (Windows 11) or Segoe MDL2
Assets (Windows 10), which ship with Windows and are not redistributed here.
