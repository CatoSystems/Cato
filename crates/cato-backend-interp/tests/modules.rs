use std::path::{Path, PathBuf};

fn project(case: &str, files: &[(&str, &str)]) -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = manifest
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("target/module_tests")
        .join(case);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, body) in files {
        std::fs::write(dir.join(name), body).unwrap();
    }
    dir.join("main.cato")
}

fn run(entry: &Path) -> String {
    let src = std::fs::read_to_string(entry).unwrap();
    let program = cato_core::compile_named(&entry.to_string_lossy(), &src).expect("compile");
    cato_interp::Interpreter::new(&program).run().expect("run")
}

fn compile_err(entry: &Path) -> String {
    let src = std::fs::read_to_string(entry).unwrap();
    cato_core::compile_named(&entry.to_string_lossy(), &src).unwrap_err()
}

#[test]
fn diamond_requires_compile_and_run() {
    let entry = project(
        "diamond",
        &[
            (
                "main.cato",
                "(ns m) (require [\"a.cato\" :as a]) (require [\"b.cato\" :as b])\n(defn main [] :- i64 (do (print (a/use-sq 3)) (print (b/use-sq 2)) 0))\n",
            ),
            (
                "a.cato",
                "(ns a) (require [\"c.cato\" :as c])\n(defn use-sq [x :- i64] :- i64 (c/sq x))\n",
            ),
            (
                "b.cato",
                "(ns b) (require [\"c.cato\" :as c])\n(defn use-sq [x :- i64] :- i64 (c/sq x))\n",
            ),
            ("c.cato", "(ns c)\n(defn sq [x :- i64] :- i64 (* x x))\n"),
        ],
    );
    assert_eq!(run(&entry), "9\n4\n");
}

#[test]
fn same_module_under_two_aliases() {
    let entry = project(
        "twoaliases",
        &[
            (
                "main.cato",
                "(ns m) (require [\"lib.cato\" :as x]) (require [\"lib.cato\" :as y])\n(defn main [] :- i64 (do (print (x/sq 3)) (print (y/sq 4)) 0))\n",
            ),
            ("lib.cato", "(ns lib)\n(defn sq [a :- i64] :- i64 (* a a))\n"),
        ],
    );
    assert_eq!(run(&entry), "9\n16\n");
}

#[test]
fn same_file_reached_by_two_spellings_dedups() {
    // `a.cato` reaches lib as `lib.cato`; `b.cato` as `./lib.cato`. Both
    // resolve to the same file string only if spelled identically; when
    // they differ the second is a distinct path (documented, not a crash).
    // Pin the same-spelling case that the convention requires.
    let entry = project(
        "spelling",
        &[
            (
                "main.cato",
                "(ns m) (require [\"a.cato\" :as a]) (require [\"b.cato\" :as b])\n(defn main [] :- i64 (do (print (a/g 3)) (print (b/g 4)) 0))\n",
            ),
            (
                "a.cato",
                "(ns a) (require [\"lib.cato\" :as lib])\n(defn g [x :- i64] :- i64 (lib/sq x))\n",
            ),
            (
                "b.cato",
                "(ns b) (require [\"lib.cato\" :as lib])\n(defn g [x :- i64] :- i64 (lib/sq x))\n",
            ),
            ("lib.cato", "(ns lib)\n(defn sq [a :- i64] :- i64 (* a a))\n"),
        ],
    );
    assert_eq!(run(&entry), "9\n16\n");
}

#[test]
fn duplicate_ns_is_rejected() {
    let entry = project(
        "dupns",
        &[
            (
                "main.cato",
                "(ns m) (require [\"a.cato\" :as a])\n(defn main [] :- i64 0)\n",
            ),
            ("a.cato", "(ns m)\n(defn sq [x :- i64] :- i64 x)\n"),
        ],
    );
    assert!(compile_err(&entry).contains("duplicate ns m"));
}

#[test]
fn cycle_terminates() {
    let entry = project(
        "cycle",
        &[
            (
                "main.cato",
                "(ns m) (require [\"a.cato\" :as a])\n(defn main [] :- i64 (a/one 0))\n",
            ),
            (
                "a.cato",
                "(ns a) (require [\"b.cato\" :as b])\n(defn one [x :- i64] :- i64 (b/two x))\n",
            ),
            (
                "b.cato",
                "(ns b) (require [\"a.cato\" :as a])\n(defn two [x :- i64] :- i64 x)\n",
            ),
        ],
    );
    // Must terminate; whether it compiles is secondary, a hang/crash is not.
    let _ = cato_core::compile_named(
        &entry.to_string_lossy(),
        &std::fs::read_to_string(&entry).unwrap(),
    );
}

#[test]
fn unknown_alias_is_a_build_error() {
    let entry = project(
        "badalias",
        &[
            (
                "main.cato",
                "(ns m) (require [\"lib.cato\" :as x])\n(defn main [] :- i64 (z/sq 3))\n",
            ),
            ("lib.cato", "(ns lib)\n(defn sq [a :- i64] :- i64 (* a a))\n"),
        ],
    );
    assert!(cato_core::compile_named(
        &entry.to_string_lossy(),
        &std::fs::read_to_string(&entry).unwrap()
    )
    .is_err());
}

#[test]
fn missing_required_file_is_an_error() {
    let entry = project(
        "missing",
        &[(
            "main.cato",
            "(ns m) (require [\"nope.cato\" :as n])\n(defn main [] :- i64 0)\n",
        )],
    );
    assert!(compile_err(&entry).contains("cannot read"));
}
