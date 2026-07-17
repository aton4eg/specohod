# A set of utilities for covering Rust projects with specification layer

* `stage1_discovery` builds a nesting graph. Not necessary.

* `stage2_static_extractor` builds metadata for each `.rs` file — a list of dependencies in `.deps.json` and a set of public interfaces in `.facts.json`. 

* `stage3_annotate` builds a dependency graph and, traversing from leaves via topological sorting, invokes an LLM to generate a specification. 
  The context consists of: 
    *   The `.rs` file code, 
    *   the `facts.json` of its dependencies, 
    *   and the specifications of its dependencies. 

    Cycles in the graph are handled by a double traversal of the cycle: during the first pass, preliminary specifications with assumptions are built, and during the second pass the assumptions are resolved.
    Additionally, a separate folder contains prompts for Kilocode so that it can start perceiving these specifications. 
    The prompts should replace the system prompts for the roles.

## Example `stage1_discovery` run
`cargo run --release -- /path/to/your/rust/project/ > /home/username/graph.json`


## Example `stage2_static_extractor` run
`cargo run --release -- --manifest /path/to/your/rust/project/Cargo.toml`


## Example `stage3_annotate` run
`cargo run --release -- --root /path/to/your/rust/project --api-key <your_api_key>`

## Notes

Prompt paths can also be customized. Defaults all look in the `prompts` folder.

The `kilocode_prompts` directory contains system prompts for Kilocode roles. When applying to your own project, adjust the base project directory in the prompt.

When working with a project, I recommend creating a guidebook using the Ask role and keeping it in the project root.

At the beginning of a Kilocode session, it is recommended to add `guidebook.md` to the context.
