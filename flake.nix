{
  description = "automerge wasm build and bindings for nix to be used with wechselbalgs wasm primop";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    wechselbalg.url = "git+file:///home/leonard/Projekte/Gestalt/wechselbalg";
  };

  outputs =
    {
      self,
      nixpkgs,
      wechselbalg,
      ...
    }:
    let
      eachSystem =
        f:
        nixpkgs.lib.genAttrs nixpkgs.lib.systems.flakeExposed (
          system:
          f {
            inherit system;
            pkgs = import nixpkgs {
              inherit system;
              overlays = [
                wechselbalg.overlays.default
                self.overlays.default
              ];
            };
          }
        );
    in
    {
      overlays.default = final: prev: {
        automerge-wasm =
          let
            wasiRust = final.pkgsCross.wasi32;
          in
          wasiRust.rustPlatform.buildRustPackage {
            pname = "automerge-wasm";
            version = "0.1.0";
            src = ./rust;

            cargoLock = {
              lockFile = ./rust/Cargo.lock;
            };

            nativeBuildInputs = [ final.lld ];

            RUSTFLAGS = "-C linker=wasm-ld -C link-arg=--export=malloc -C link-arg=--export=free";

            preConfigure = ''
              export CARGO_TARGET_WASM32_WASIP1_LINKER="wasm-ld"
              export CARGO_TARGET_WASM32_UNKNOWN_WASIP1_LINKER="wasm-ld"
            '';

            installPhase = ''
              mkdir -p $out
              cp target/wasm32-wasip1/release/*.wasm $out/automerge.wasm
              ${final.binaryen}/bin/wasm-opt -Oz $out/automerge.wasm -o $out/automerge.wasm
            '';
          };

        lib = (prev.lib or { }) // {
          automerge =
            let
              b64Wasm = builtins.readFile (
                final.runCommand "automerge-base64" { } ''
                  base64 -w0 "${final.automerge-wasm}/automerge.wasm" > $out
                ''
              );

              # Safe call wrapper: intercepts { success, data, error } envelope
              callWasm = fn: args:
                let
                  res = final.lib.wasm {
                    bytecode = b64Wasm;
                    function = fn;
                  } args;
                in
                if res.success then
                  res.data
                else
                  throw "automerge-nix [${fn}]: ${res.error}";

              normPath = p: if builtins.isList p then p else [ p ];

              dHelpers = {
                put = path: value: {
                  action = "put";
                  path = normPath path;
                  inherit value;
                };
                counter = path: value: {
                  action = "counter";
                  path = normPath path;
                  inherit value;
                };
                insert = path: index: value: {
                  action = "insert";
                  path = normPath path;
                  inherit index value;
                };
                delete = path: {
                  action = "delete";
                  path = normPath path;
                };
                inc = path: value: {
                  action = "inc";
                  path = normPath path;
                  inherit value;
                };
                spliceText = path: index: del: text: {
                  action = "splicetext";
                  path = normPath path;
                  inherit index del text;
                };
                spliceList = path: index: del: values: {
                  action = "splicelist";
                  path = normPath path;
                  inherit index del values;
                };
                mark = path: start: end: name: value: {
                  action = "mark";
                  path = normPath path;
                  inherit start end name value;
                };
                unmark = path: start: end: name: {
                  action = "unmark";
                  path = normPath path;
                  inherit start end name;
                };
              };

              init =
                {
                  actor ? null,
                  entropy ? null,
                }:
                let
                  resolvedActor =
                    if actor != null then
                      actor
                    else if entropy != null then
                      builtins.hashString "sha256" (builtins.toJSON entropy)
                    else
                      throw "automerge.init: you must provide either 'actor' or 'entropy'";
                in
                {
                  actor = resolvedActor;

                  new =
                    { }:
                    (callWasm "automerge_init_doc" {
                      actor = resolvedActor;
                    }).doc;

                  # Polymorphic change: handles both 2 and 3 argument forms
                  change =
                    doc: arg2:
                    let
                      hasOptions = builtins.isAttrs arg2 && !builtins.isFunction arg2;
                    in
                    if hasOptions then
                      changeFn:
                        (callWasm "automerge_change" {
                          inherit doc;
                          actor = resolvedActor;
                          actions = changeFn dHelpers;
                          message = arg2.message or null;
                          time = arg2.time or null;
                        }).doc
                    else
                      (callWasm "automerge_change" {
                        inherit doc;
                        actor = resolvedActor;
                        actions = arg2 dHelpers;
                        message = null;
                        time = null;
                      }).doc;

                  merge =
                    doc1: doc2:
                    (callWasm "automerge_merge" {
                      inherit doc1 doc2;
                    }).doc;

                  # --- 2-Way Sync Protocol (P2P / Veilid) ---

                  # Create new blank sync state for a remote peer
                  syncNewState =
                    { }:
                    callWasm "automerge_sync_state_new" { };

                  # Generate outbound sync message: returns { doc, sync_state, message }
                  syncGenerateMessage =
                    { doc, syncState }:
                    callWasm "automerge_sync_generate_message" {
                      inherit doc;
                      sync_state = syncState;
                    };

                  # Apply inbound sync message from remote peer: returns { doc, sync_state }
                  syncReceiveMessage =
                    { doc, syncState, message }:
                    callWasm "automerge_sync_receive_message" {
                      inherit doc;
                      sync_state = syncState;
                      inherit message;
                    };

                  # --- Automerge Native Diff Engine ---
                  diff =
                    { doc, before, after }:
                    callWasm "automerge_diff" {
                      inherit doc before after;
                    };

                  # Targeted subtree/field read
                  get =
                    doc: path:
                    callWasm "automerge_get" {
                      inherit doc;
                      path = normPath path;
                    };

                  # Inspect Rich Text marks on a Text object
                  getMarks =
                    doc: path:
                    callWasm "automerge_get_marks" {
                      inherit doc;
                      path = normPath path;
                    };

                  heads =
                    doc:
                    callWasm "automerge_heads" {
                      inherit doc;
                    };

                  history =
                    doc:
                    callWasm "automerge_history" {
                      inherit doc;
                    };

                  forkAt =
                    doc: heads:
                    (callWasm "automerge_fork_at" {
                      inherit doc heads;
                    }).doc;

                  conflicts =
                    doc: path:
                    callWasm "automerge_conflicts" {
                      inherit doc;
                      path = normPath path;
                    };

                  materialize =
                    doc:
                    callWasm "automerge_materialize" {
                      inherit doc;
                    };

                  withActor = newActor: init { actor = newActor; };
                };
            in
            {
              inherit init;
            };
        };
      };

      packages = eachSystem ({ pkgs, ... }: {
        default = pkgs.automerge-wasm;
      });

      checks = eachSystem (
        { pkgs, ... }:
        {
          automerge-simulation =
            let
              simulationNix = nameA: nameB:
                let
                  alice = pkgs.lib.automerge.init { actor = nameA; };
                  bob = pkgs.lib.automerge.init { actor = nameB; };

                  # 1. Base doc with Rich Text Marks
                  doc0 = alice.new { };
                  doc1 = alice.change doc0 { message = "Initial commit"; } (d: [
                    (d.put "message" "hello p2p")
                    (d.counter "views" 10)
                    (d.spliceText "doc" 0 0 "Bold text here")
                    (d.mark "doc" 0 4 "bold" true)
                  ]);

                  # Record heads before divergence
                  headsBefore = alice.heads doc1;

                  # 2. Divergent concurrent edits
                  docAlice = alice.change doc1 (d: [
                    (d.inc "views" 5)
                    (d.spliceText "doc" 14 0 " - edited by Alice")
                  ]);

                  docBob = bob.change doc1 (d: [
                    (d.inc "views" 20)
                    (d.spliceText "doc" 0 0 "Intro: ")
                  ]);

                  headsAfterAlice = alice.heads docAlice;

                  # 3. Native Automerge Diff
                  aliceDiff = alice.diff {
                    doc = docAlice;
                    before = headsBefore;
                    after = headsAfterAlice;
                  };

                  # 4. Two-Way Sync Protocol Simulation (Veilid round-trip)
                  # Alice initializes a sync state for Bob; Bob initializes one for Alice
                  stateA0 = alice.syncNewState { };
                  stateB0 = bob.syncNewState { };

                  # Round 1: Alice generates sync message for Bob
                  syncA1 = alice.syncGenerateMessage { doc = docAlice; syncState = stateA0; };
                  syncB1 = bob.syncGenerateMessage { doc = docBob; syncState = stateB0; };

                  # Round 1 delivery
                  syncB2 = bob.syncReceiveMessage { doc = syncB1.doc; syncState = syncB1.sync_state; message = syncA1.message; };
                  syncA2 = alice.syncReceiveMessage { doc = syncA1.doc; syncState = syncA1.sync_state; message = syncB1.message; };

                  # Round 2 acknowledgement and remaining changes
                  syncA3 = alice.syncGenerateMessage { doc = syncA2.doc; syncState = syncA2.sync_state; };
                  syncB3 = bob.syncGenerateMessage { doc = syncB2.doc; syncState = syncB2.sync_state; };
                  syncB4 = bob.syncReceiveMessage { doc = syncB3.doc; syncState = syncB3.sync_state; message = syncA3.message; };
                  syncA4 = alice.syncReceiveMessage { doc = syncA3.doc; syncState = syncA3.sync_state; message = syncB3.message; };

                  # 5. Targeted Reads & Marks
                  targetedViews = alice.get syncA4.doc "views";
                  textMarks = alice.getMarks syncA4.doc "doc";
                in
                {
                  aliceFinal = alice.materialize syncA4.doc;
                  bobFinal = bob.materialize syncB4.doc;
                  diffCount = builtins.length aliceDiff;
                  inherit targetedViews;
                  marksCount = builtins.length textMarks;
                };

              testScript = pkgs.writeText "test-automerge.js" ''
                ${pkgs.lib.jsRuntime}
                const assert = require("assert");

                async function test() {
                  console.log("Evaluating P2P 2-Way Sync & Automerge Engine...");

                  const resVal = (${pkgs.lib.toJS simulationNix}.call(${pkgs.lib.toJS "alice"}).call(${pkgs.lib.toJS "bob"}));

                  let rawJson;
                  if (resVal && typeof resVal.toJSON_ === "function") {
                    rawJson = resVal.toJSON_().asString();
                  } else if (resVal && typeof resVal.asString === "function") {
                    rawJson = resVal.asString();
                  } else {
                    rawJson = JSON.stringify(resVal);
                  }

                  const data = typeof rawJson === "string" ? JSON.parse(rawJson) : rawJson;
                  console.log("Simulation Result:\n", JSON.stringify(data, null, 2));

                  // Assert P2P convergence: both peers converged to identical states!
                  assert.deepStrictEqual(data.aliceFinal, data.bobFinal, "Peers must converge after 2-way sync!");

                  // Counter was incremented by both: 10 + 5 + 20 = 35
                  assert.strictEqual(data.aliceFinal.views, 35);
                  assert.strictEqual(data.targetedViews, 35);

                  // Spliced text merged concurrently
                  assert(data.aliceFinal.doc.includes("Intro: "));
                  assert(data.aliceFinal.doc.includes("Bold text"));
                  assert(data.aliceFinal.doc.includes("Alice"));

                  // Marks were tracked
                  assert.strictEqual(data.marksCount, 1);

                  // Diff returned patches
                  assert(data.diffCount >= 1, "Diff must contain patches");

                  console.log("\n✅ All P2P Sync, Marks, Diff, and Targeted Read tests passed!");
                }

                test().catch(err => {
                  console.error("\n❌ Test failed with error:\n", err);
                  process.exit(1);
                });
              '';
            in
            pkgs.runCommand "test-automerge-simulation"
              {
                nativeBuildInputs = [ pkgs.nodejs ];
              }
              ''
                node ${testScript}
                mkdir -p $out
                touch $out/success
              '';
        }
      );
    };
}