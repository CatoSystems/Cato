# Cato Generics, Common Lisp Style — Design Study

*Status: **implemented**. This document specifies how CLOS-style generic
functions adapt to Cato's world: closed ADTs, static types,
monomorphization, no runtime class hierarchy. The 2026-09-14 refinement
replaces declaration-order specificity with a structural partial order;
the text reflects the implemented semantics.*

## 1. What Common Lisp gives you

CLOS generics are *multi-methods with an open world*:

```lisp
(defgeneric area (shape) (:documentation "area of a shape"))
(defmethod area ((s circle)) (* pi (circle-radius s)))
(defmethod area ((s rect)) (* (rect-w) (rect-h)))
(area (make-circle :r 2))     ; dispatch on runtime class
(call-next-method)            ; invoke the next-most-specific method
```

The essential properties:

1. **One name, many bodies.** `defgeneric` declares a function; each
   `defmethod` adds a specialization. Methods live in separate definitions
   — new behavior attaches to existing types without editing them.
2. **Runtime dispatch on the actual arguments** (multiple dispatch when
   several parameters are specialized), choosing the *most specific*
   applicable method by class precedence.
3. **`call-next-method`** — a method invokes whatever dispatch would have
   chosen next.
4. **Method combination** (`:around`/`:before`/`:after`) — optional
   composition protocol.
5. **Open world** — new methods can be added at runtime, so dispatch is a
   runtime search and dispatch failure is a runtime error.

## 2. What must change for Cato

Cato has no classes, no inheritance, and no runtime type information.
What it has instead: closed ADTs (a `deftype` enumerates its variants at
compile time; the tag *is* the type discriminant), static types with
monomorphization, and a compile-time totality discipline (exhaustive
`match`). The honest translation keeps properties 1–3 and trades property
5 for the systems promise:

| CLOS                              | Cato adaptation                                             |
|-----------------------------------|-------------------------------------------------------------|
| class specializer                | ADT **variant** specializer (`(defmethod area [(Circle r)] ...)`) |
| class precedence list            | **structural partial order**: `Variant > Adt`, component-wise for multiple dispatch; declaration order is semantically irrelevant |
| runtime dispatch search          | compile-time **exhaustive tag dispatch** (a `match`), zero overhead |
| open world, runtime failure      | closed world; coverage checked at compile time like `match`     |
| `call-next-method`               | a **direct call** to the next method body (statically known)    |
| `:before`/`:after` combination   | deferred (see §7)                                            |

The key insight: **a Cato generic function over one specialized ADT
parameter *is* a `match` over that ADT's variants** — with arms spread
across multiple `defmethod` forms instead of one site. Everything else is
syntax and a coverage check we already have.

## 2b. The specificity relation (normative)

For one component, `s ⪰ t` ("s is at least as specific as t") iff
`s == t`, or `s = Variant(v)` and `t = Adt(A)` with `v ∈ A`. A method
tuple `m' ⪰ m` iff `m'.i ⪰ m.i` for every component. At a given call,
applicable methods `m` with some other applicable `m' ⪰ m` strictly are
dominated; the **maximal set** under this partial order must have exactly
one element:

- **empty** → compile error `E204` (uncovered combination),
- **one** → that method runs,
- **two or more** → compile error `E203` (ambiguous: neither is more
  specific; the error names the competing methods). Adding a strictly
  more specific method resolves the ambiguity.

Declaration order is never consulted for semantics. A method dominated at
*every* combination it applies to is **unreachable** — compile error
`E205` (as are duplicate specializer tuples). Resolution is
demand-driven: generics without calls are only checked for duplicates and
unreachability, not coverage.

## 3. Proposed surface syntax

Cato syntax stays S-expression-first, `:-` for types:

```clojure
(deftype Shape
  (Circle [r :- f64])
  (Rect [w :- f64 h :- f64])
  (Tri [a :- f64 b :- f64 c :- f64]))

(defgeneric area [s :- Shape] :- f64)          ; declaration only

(defmethod area [s :- Circle] :- f64
  (* 3.14159265358979 (* s.r s.r)))            ; see §6 on field access

(defmethod area [s :- Rect] :- f64
  (* s.w s.h))

(defmethod area [s :- Shape] :- f64            ; the *default* method:
  0.0)                                         ; specializer = whole ADT
```

Call sites are ordinary: `(area (Circle 2.0))` — dispatch is invisible.

Multiple dispatch:

```clojure
(defgeneric collide [a :- Entity b :- Entity] :- f64)
(defmethod collide [a :- Bullet b :- Asteroid] :- f64 ...)
(defmethod collide [a :- Entity  b :- Entity]  :- f64 ...)  ; default pair
```

## 4. Lowering: desugar to what already exists

### Single dispatch

```
(defgeneric f [x :- A] :- T)
(defmethod f [x :- V1] body1)
(defmethod f [x :- V2] body2)
(defmethod f [x :- A]  default)
```

desugars **exactly** to today's HIR:

```
(defn f [x :- A] :- T
  (match x
    (V1 [x1] (let [x x1] body1))
    (V2 [x2] (let [x x2] body2))
    (_       default)))
```

- Coverage rule (compile error when violated): every variant either has a
  method or a whole-ADT default exists. This is the existing
  exhaustiveness check, reused.
- Multiple `defmethod`s for the same variant: compile error in the first
  slice ("duplicate method for Circle") — declaration-order precedence is
  only needed once `call-next-method` chains exist (§5).
- Variant binders: `(defmethod f [(Circle r)] ...)` binds fields
  positionally like `match`; `(defmethod f [s :- Circle] ...)` binds the
  whole value. Both forms may coexist: the whole-value form is sugar for
  the binder form plus a `let`.

Cost: identical to hand-written `match` — one tag compare and a branch
(or a jump table after LLVM optimization). No allocation, no vtables, no
runtime metadata. This is the "pay nothing for what you don't dispatch"
property worth keeping from the LLVM story.

### Multiple dispatch

Two specialized parameters lower to a **nested match**: outer on `a`'s
tag, inner on `b`'s tag within each branch. The coverage rule generalizes
to the cross product: every (variantᵃ × variantᵇ) pair must be covered by
some method (specific pair or a default whose specializer subsumes it).
The checker is set-cover over the declared specializers — small, finite,
compile-time. Codegen cost is O(k) compares for k = sum of inner cases;
LLVM turns dense dispatches into tables.

For three-plus parameters the nested-match encoding stays linear in the
product size; real programs specialize one or two parameters, so this is
fine, but §7 notes the table alternative.

## 5. `call-next-method`

Because dispatch is static, the "next method" for a given arm is known at
compile time: the *previous* declared method whose specializer matches
this arm's variant, or the default. `(call-next-method)` inside a method
body desugars to a direct call to that method's body (extracted as a
hidden `defn f__m3` per method, with the generic `f` as the public
dispatcher):

```
(defmethod f [x :- V1] (call-next-method))  →
(defn f__m1 [x :- V1] :- T (f__default x))  ; next = default for V1
```

Chained specificity then falls out of *reverse declaration order*: later
methods are "more specific", and `call-next-method` walks toward earlier
ones. Duplicate-variant methods become legal again (they form the chain).

## 6. Interactions to design carefully

- **ADTs are closed but defined anywhere.** Methods on `Shape` may appear
  in any file that can name `Shape` (after `require`). Adding a variant to
  `deftype Shape` instantly makes every generic over it a compile error
  until covered — the open world's *benefit* (find all affected code)
  with none of the runtime cost.
- **Fields.** The examples above use `s.r`. Today's slice has no field
  accessors; method bodies would use `match` to destructure. Two options:
  (a) require destructuring binders in methods (no new feature);
  (b) add dot-access `(get-field s r)` as sugar for a single-variant
  match. Option (a) is the honest first slice; (b) is a separate, small
  feature with its own design questions.
- **Generics over type parameters** (`(Vector T)`, monomorphized defns):
  a `defmethod` whose specializer mentions `T` becomes a *fallback for
  any variant not otherwise covered* — this composes the existing
  monomorphization with variant dispatch and needs a precedence rule
  (concrete variants beat `T` always). Defer to slice 2; slice 1 keeps
  specializers concrete.
- **Ownership.** Methods take their parameters the same way `defn`s do
  (`:-`, `&`, `&!`). Dispatching a `match` on a value borrows it, so a
  generic over a non-Copy ADT keeps ownership unchanged; a by-value
  param moves into the chosen arm exactly like `match` does today.
- **First-class generics?** A generic's name could be passed as
  `(Fn ...)` only if it has exactly one method (then it IS that method).
  Otherwise it's a dispatch, not a closure — reject with a clear error.
  Revisit when generics meet first-class closures.

## 7. Deferred pieces

- **Method combination** (`:around`/`:before`/`:after`): expressible as
  desugared nesting once `call-next-method` exists (`:before/:after` =
  sequencing inside the dispatching body). Design then, not now.
- **Dispatch tables**: if any real program's nested dispatch gets wide,
  emit a `getelementptr` jump table over (tagᵃ × tagᵇ). Purely a codegen
  refinement; the HIR does not change.
- **Open-world integration** (methods defined in separately-loaded
  modules discoverable at runtime): rejected as a goal — it costs the
  runtime search Cato exists to avoid.

## 8. Slice plan (TDD-sized)

1. ✅ `defgeneric` + single-ADT `defmethod` resolved to `match`; coverage
   errors; duplicate errors. Reference: `examples/generics.cato` (shapes,
   both backends via the parity suite).
2. ✅ Whole-value and destructuring specializer forms.
3. ⬜ `call-next-method` — **deferred** until the basic specificity
   system is exercised; when built it follows the structural order (most
   specific → next less specific → fallback), not declaration order.
4. ✅ Two-parameter multiple dispatch: component-wise specificity,
   cross-product coverage, `E203` ambiguity on incomparable maxima.
5. ⬜ (later) `T` fallback methods; method combination; jump tables.

Total new machinery: one collection pass (form-level, like macro
expansion), a resolution phase at call sites (applicability → maximal
set under the partial order → coverage/ambiguity), and synthesized
dispatch defns that are ordinary HIR. Zero codegen changes — the LLVM
backend and the interpreter see plain defns and matches. That is the
argument for doing generics this way: CLOS ergonomics, match economics.

## 9. `print` is a generic

The prelude defines print as a multimethod over the builtin types
(`i64`, all widths, `f64`, `str`, `bool`); the method bodies call the
print runtime primitives. Builtin dispatch is static (exact type match),
so `(print x)` on a scalar compiles to a direct hidden-method call — no
tree. `(print x)` on an ADT builds the dispatch tree over its variants.
The per-type builtins remain as primitives but user code (all of
`examples/`) uses `(print ...)`.
