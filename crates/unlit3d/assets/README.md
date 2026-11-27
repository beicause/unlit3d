# Bundled assets

## NotoSansSC-Regular-subset.woff2

The Chinese font a page and an overlay are laid out with when `sc_fonts` is on.
It is
Noto Sans SC instantiated at weight 400 and subset to:

- every character of the 3500 level-1 and 3000 level-2 entries of the Table of
  General Standard Chinese Characters (通用规范汉字表),
- printable ASCII, U+0020 to U+007E,
- CJK punctuation, U+3000 to U+303F,
- fullwidth forms, U+FF01 to U+FF5E.

The 3.5 MB of level-3 characters and everything outside those blocks are left
out: the font ships 7101 glyphs where Noto Sans SC ships 31036, which is what
keeps the file at 977 kB instead of 10.6 MB. A page that needs a character
outside the subset paints a missing glyph rather than the character, so a page
outside this range turns `blitz_system_fonts` on instead.

Licensed under the SIL Open Font License 1.1; the license is
[NotoSansSC-OFL.txt](./NotoSansSC-OFL.txt), as the license requires it to travel
with the font.
