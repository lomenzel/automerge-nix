# ATTENTION: ITS MOSTLY AI GENERATED

# automerge-nix

**WebAssembly build and pure Nix bindings for [Automerge](https://automerge.org/) (v0.12), designed for [wechselbalg](https://github.com/lomenzel/wechselbalg) and Nix-based application frameworks.**

`automerge-nix` bridges the official Rust Automerge CRDT engine into Nix environments compiled to JavaScript via `wechselbalg`'s WASM primop. It allows you to build collaborative, local-first applications where document mutations, conflict resolution, character-level text splicing, and vector-clock branching can be expressed directly in pure Nix code.

---

## ✨ Features

- **Full Automerge Engine:** Powered by `automerge` 0.12 compiled to freestanding WebAssembly targeting WASI (`wasm32-wasip1`).
- **Action DSL:** Operation builders (`put`, `counter`, `inc`, `spliceText`, `spliceList`, `insert`, `delete`) matching the upstream Rust `Transactable` API.
- **Collaborative Text Editing:** Character-by-character text splicing without Last-Write-Wins overwrite issues.
- **CRDT Counters:** Additive conflict-free counters that merge without losing concurrent increments.
- **Actor-Curried Instances:** Bind client identity (`actor` or `entropy`) once at initialization without polluting mutation signatures.
- **Incremental Sync Protocol:** Generate and apply compact binary deltas (`saveIncremental` / `applyIncremental`) for low-bandwidth WebSockets/WebRTC communication.
- **Time Travel & DAG Inspection:** Inspect commit histories (`history`), vector-clock heads (`heads`), checkout historical points in time (`forkAt`), and inspect concurrent conflicting registers (`conflicts`).
- **Nix-Native Materialization:** Extract document snapshots directly into native Nix attribute sets, lists, strings, and numbers.

---

## 🚀 Quick Start

```nix
let
  # 1. Initialize peers with distinct identities
  alice = pkgs.lib.automerge.init { actor = "alice"; };
  bob   = pkgs.lib.automerge.init { actor = "bob"; };

  # 2. Alice creates a base document
  doc0 = alice.new { };
  doc1 = alice.change doc0 { message = "Initial commit"; } (d: [
    (d.put "title" "Nix CRDT Guide")
    (d.counter "views" 0)
    (d.spliceText "body" 0 0 "Hello World")
    (d.put "tags" [ ])
    (d.insert "tags" 0 "crdt")
  ]);

  # 3. Concurrent edits: Bob and Alice edit simultaneously
  docBob = bob.change doc1 (d: [
    (d.inc "views" 10)
    (d.spliceText "body" 6 0 "Awesome ") # "Hello Awesome World"
    (d.insert "tags" 1 "wasm")
  ]);

  docAlice = alice.change doc1 (d: [
    (d.inc "views" 5)
    (d.spliceText "body" 11 0 "!")       # "Hello World!"
    (d.insert "tags" 1 "nix")
  ]);

  # 4. Deterministic merge (both increments and text insertions survive!)
  mergedDoc = alice.merge docAlice docBob;
in
  # 5. Extract native Nix data
  alice.materialize mergedDoc
  /*
  {
    title = "Nix CRDT Guide";
    views = 15;
    body  = "Hello Awesome World!";
    tags  = [ "crdt" "nix" "wasm" ];
  }
  */
```

---

## 📦 Installation

Add `automerge-nix` to your `flake.nix`:

```nix
{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    wechselbalg.url = "git+file:///path/to/wechselbalg";
    automerge-nix.url = "git+file:///path/to/automerge-nix";
  };

  outputs = { nixpkgs, wechselbalg, automerge-nix, ... }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        overlays = [
          wechselbalg.overlays.default
          automerge-nix.overlays.default
        ];
      };
    in {
      # pkgs.lib.automerge is now available!
    };
}
```

---

## 📖 API Reference

### 1. Instance Initialization

```nix
am = pkgs.lib.automerge.init {
  actor ? null,    # Explicit writer identity (e.g. "user-123")
  entropy ? null,  # Optional seed or UUID; hashed to derive deterministic actor ID
};
```

---

### 2. Document Operations

#### `am.new { }`
Creates a fresh, empty Automerge document initialized with the curried actor ID. Returns an opaque base64 document string.

#### `am.change doc [options] (d: [ ... ])`
Applies a batch of atomic operations inside a single Automerge transaction. 

Supports optional Git-like commit metadata:
```nix
# Without options:
doc1 = am.change doc0 (d: [ ... ]);

# With options:
doc1 = am.change doc0 { 
  message = "Add user profile"; 
  time = 1710000000; 
} (d: [ ... ]);
```

#### `am.merge doc1 doc2`
Merges two document states deterministically according to CRDT rules.

#### `am.materialize doc`
Converts the internal Automerge CRDT state into a standard Nix attribute set (JSON-compatible primitives, lists, and maps).

#### `am.withActor newActor`
Forks the client instance under a new actor ID while keeping the same module configuration.

---

### 3. Action DSL (`d`)

Inside `am.change`, the callback receives the `d` helper library. All paths can be single keys or nested lists with map strings and numeric list indices (`[ "todos" 0 "title" ]`):

| Action | Description | Example |
| :--- | :--- | :--- |
| **`d.put`** | Set a value in a map or replace an item in a list | `d.put "title" "My Post"`<br>`d.put [ "config" "debug" ] true` |
| **`d.counter`** | Initialize an additive CRDT Counter | `d.counter "views" 0` |
| **`d.inc`** | Increment/decrement a CRDT Counter | `d.inc "views" 1`<br>`d.inc "views" (-5)` |
| **`d.insert`** | Insert an item into a list at an index | `d.insert "tags" 0 "nix"` |
| **`d.delete`** | Delete a key from a map or an item from a list | `d.delete "obsoleteKey"`<br>`d.delete [ "items" 2 ]` |
| **`d.spliceText`** | Collaborative character-level text splice | `d.spliceText "body" index delCount "text"` |
| **`d.spliceList`** | Splice a list (delete and insert multiple items) | `d.spliceList "tags" index delCount [ "a" "b" ]` |

---

### 4. Networking & Syncing

For distributed or client-server apps, sending the entire document on every keystroke is inefficient. Use incremental deltas:

```nix
# 1. Local peer commits and extracts only the delta bytes
delta = alice.saveIncremental docAlice;
# delta is: { doc = "<updatedDoc>"; changes = "<base64Delta>"; }

# 2. Remote peer applies only the binary delta
docBobUpdated = bob.applyIncremental docBob delta.changes;
```

---

### 5. DAG History, Inspection & Time Travel

Because Automerge is an append-only DAG of changes:

```nix
# Current vector-clock heads (array of hex strings)
heads = am.heads doc;

# Full Git-like commit log
# [ { hash = "..."; actor = "..."; seq = 1; message = "..."; time = 123; deps = [ ... ]; } ]
log = am.history doc;

# Time travel: checkout document as it existed at a past head
historicalDoc = am.forkAt doc [ "4a2b9f..." ];

# Conflict inspection: view competing concurrent writes for a given key
# Returns list of all concurrent values (if multiple peers wrote simultaneously)
conflicts = am.conflicts doc "title";
```

---

## 🛠️ Architecture & Under the Hood

```
   ┌──────────────────────────────────────────────┐
   │        Nix Application Code / UI             │
   │      (am.change, am.merge, d.spliceText)     │
   └──────────────────────┬───────────────────────┘
                          │
                   lib.toJS / Primop
                          │
   ┌──────────────────────▼───────────────────────┐
   │     Wechselbalg JavaScript Runtime           │
   │    (Memory allocation, JSON marshalling)     │
   └──────────────────────┬───────────────────────┘
                          │  malloc / free (linear memory)
   ┌──────────────────────▼───────────────────────┐
   │         automerge-wasm (WASI)                │
   │     Rust core compiled via wasm32-wasip1     │
   │     (automerge-rs 0.12 + serde_json)         │
   └──────────────────────────────────────────────┘
```

1. **Rust Crate (`rust/src/lib.rs`):** Wraps `automerge::AutoCommit` in a C-ABI export layer with freestanding memory wrappers (`malloc` / `free`).
2. **Nixpkgs WASI Toolchain:** Built with `pkgsCross.wasi32.rustPlatform.buildRustPackage`, linked via `wasm-ld`, and optimized with `wasm-opt -Oz`.
3. **Data Marshalling:** Functions receive and emit JSON payloads over WASM linear memory, automatically deserialized into native Nix values by Wechselbalg's `lib.wasm` primop.

---

## 🧪 Testing & Verification

The repository includes an end-to-end integration test that simulates concurrent multi-peer editing, text splicing, incremental syncing, and CRDT verification inside Node.js:

```bash
# Run all checks via your Nix flake
nix flake check

# Or build the check derivation directly
nix build .#checks.x86_64-linux.automerge-simulation -L
```
