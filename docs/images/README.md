# Documentation images

`social-preview.png` is a 1280 × 640 GitHub repository cover exported from
[Repo Thumb](https://repothumb.coolstuff.work/). Its editable settings are saved in
[social-preview.repo-thumb.json](social-preview.repo-thumb.json); import this
recipe into Repo Thumb to regenerate the cover. It uses Geist, Shadowroom's dark
background and blue accent, and the Rust language mark.

`review.png`, `compare.png`, `catalog.png`, `light.png`, and `preferences.png` are
actual captures of Shadowroom's native GPUI window on Linux X11/Xwayland at
1440 × 900, captured on 2026-10-26. No interface elements were composited or
redrawn. The catalog is a private demo profile, separate from the user's library.
[Provenance](provenance.json) records the executable and image SHA-256 hashes;
source photo bytes and modification times were unchanged after capture.

Reproduce with ImageMagick and xdotool installed, a built release binary, and a
folder of demo photos:

```sh
uv run --no-project python scripts/capture-screenshots.py --photos /path/to/demo-photos
```

The example photos come from [Unsplash](https://unsplash.com/), under the
[Unsplash License](https://unsplash.com/license). They appear only within these
application screenshots; full-size source photographs are not bundled with the
application or source release.

| Demo filename | Original image |
| --- | --- |
| `01-alpine.jpg` | [Alpine landscape](https://images.unsplash.com/photo-1464822759023-fed622ff2c3b) |
| `02-lakeside.jpg` | [Lakeside cabin](https://images.unsplash.com/photo-1470770841072-f978cf4d019e) |
| `03-forest.jpg` | [Forest](https://images.unsplash.com/photo-1441974231531-c6227db76b6e) |
| `04-valley.jpg` | [Mountain lake](https://images.unsplash.com/photo-1501785888041-af3ef285b470) |
| `05-wildlife.jpg` | [Mountain landscape](https://images.unsplash.com/photo-1472396961693-142e6e269027) |
| `06-ridge.jpg` | [Sunlit ridge](https://images.unsplash.com/photo-1469474968028-56623f02e42e) |
