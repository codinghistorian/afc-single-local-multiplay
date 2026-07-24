# Asset provenance

## Kenney Cube Pets 2.0

The fighter models and HUD portraits come from Kenney's **Cube Pets 2.0**
package:

- Official package page: <https://kenney.nl/assets/cube-pets>
- Package version: 2.0 ("Complete remake, added animals & animations")
- Creator/distributor: Kenney
- License: [Creative Commons Zero 1.0 Universal (CC0)](https://creativecommons.org/publicdomain/zero/1.0/)
- Repository license copy:
  [`assets/characters/kenney_cube_pets/License.txt`](../assets/characters/kenney_cube_pets/License.txt)

Kenney's package page identifies Cube Pets as CC0, and
[Kenney's support page](https://kenney.nl/support) confirms that asset-page
downloads may be used in commercial projects and do not require attribution.
The package's included license file explicitly permits personal, educational,
and commercial use.

The HUD files are the package's unmodified `Previews/animal-*.png` images.
Their committed SHA-256 values are:

| Repository path | SHA-256 |
| --- | --- |
| `assets/ui/hud/animal-bee.png` | `010f225dc24ca7e960718d2b6a6cf12c90ca40521294d193b8703b57ea77dc77` |
| `assets/ui/hud/animal-cat.png` | `fdcb39a1998d5cc411ac13342c5d772e70bee3e90dda029e44d54c283edfd9af` |
| `assets/ui/hud/animal-chick.png` | `f5009d3e444075d6319d30fd9cabedfb17e650ae2720d6ba624dd311ccb52cea` |
| `assets/ui/hud/animal-dog.png` | `d40a34a1980ae0a6f3ce204da196185d163b2836f642403350b8125f05f537fe` |
| `assets/ui/hud/animal-fox.png` | `8860030606464cab918c5c1af2c0ba79981101bcdaf0b88747684a623288ea63` |
| `assets/ui/hud/animal-panda.png` | `2d999b2db9fad0482ca9201429ce1c05b7448e158f3a9da57a24a66f980a3124` |
| `assets/ui/hud/animal-penguin.png` | `c3cab0b4424661b59389b85e5d6e637202b3b958e897a555288f80d6c5685b71` |
| `assets/ui/hud/animal-pig.png` | `374b623a33fe84516faab08acf0cfe0e816e6d264a283fa76a455fc54ece5840` |

Verify the committed portraits with:

```sh
shasum -a 256 assets/ui/hud/animal-*.png
```

Provenance was re-verified against the official package and its included
license on 2026-07-23.
