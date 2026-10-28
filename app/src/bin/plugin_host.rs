//! `merlin-plugin-host`: runs one plugin in a process of its own, so that a plugin that crashes
//! or hangs cannot take the app down. The app starts it (see `merlin_sandbox`).

fn main() {
    merlin_sandbox::host::main()
}
