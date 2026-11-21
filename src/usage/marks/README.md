# Usage pane artwork

- **Codex:** OpenAI's [Codex app icon](https://images.ctfassets.net/8su2tbn87fck/37ep8OPlSuUYpcTFkkW9NO/1a6381ac6612a83ec6d07b3fac5c5228/Blossom_4k_Icon_1.png) from the [official Codex page](https://chatgpt.com/codex). OpenAI publishes this artwork as PNG, so `codex.svg` wraps the raster mark without tracing it.
- **Claude:** “Claude Spark – Clay” from the [Anthropic press kit](https://www.anthropic.com/press-kit) ([ZIP](https://assets.claude.com/ae59ca4ca194dac9c9dc3bc78c5829468cb0e8af.zip)).
- **DeepSeek, Devin, OpenRouter, GitHub Copilot and OpenCode:** resized from [LobeHub Icons](https://github.com/lobehub/lobe-icons/tree/9752d967494e48d8f83f2808feb2b0207bf70997/packages/static-png/light) under the [MIT license](LobeHub-LICENSE.txt). The menu uses the monochrome marks as templates so they follow the system appearance.

The logos are trademarks of their respective owners, used here to identify the tools. The embedded PNGs are 192×192 px with transparent backgrounds.

The figures use [Inter 4.1](https://github.com/rsms/inter/tree/v4.1), weight 750, under the [SIL Open Font License 1.1](Inter-OFL.txt). Run `python3 generate-figures.py` with Pillow installed to download the pinned font and regenerate `figures.alpha`. Pillow is only used to generate the artwork, not to build or run milo.

`figures.alpha` holds one compressed, 4-bit alpha mask per possible figure. The pane reuses the compressed pixels with a different PNG palette for each usage level and terminal background.
