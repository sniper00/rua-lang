# moon_rs external-library smoke workspace

Open this directory itself as a VS Code workspace. `.ruarc.toml` adds the
sibling `moon_rs/lualib/moon.ruai` declaration as an external library.

In `main.rua`, `moon::` provides member completion. Hovering or navigating from
an API call such as `moon::query` opens the declaration in the sibling
`moon_rs` checkout.

The same project configuration is consumed by `ruac`:

```sh
../../target/release/ruac build main.rua
```
