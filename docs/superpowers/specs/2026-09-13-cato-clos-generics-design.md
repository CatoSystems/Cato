# Cato Generics, Common Lisp Style — Design Study

*Status: design only, not yet built. This document studies how CLOS-style
generic functions adapt to Cato's world: closed ADTs, static types,
monomorphization, no runtime class hierarchy.*

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
| class precedence list            | declaration order (last applicable wins — most recent refinement) |
| runtime dispatch search          | compile-time **exhaustive tag dispatch** (a `match`), zero overhead |
| open world, runtime failure      | closed world; coverage checked at compile time like `match`     |
| `call-next-method`               | a **direct call** to the next method body (statically known)    |
| `:before`/`:after` combination   | deferred (see §7)                                            |

The key insight: **a Cato generic function over one specialized ADT
parameter *is* a `match` over that ADT's variants** — with arms spread
across multiple `defmethod` forms instead of one site. Everything else is
syntax and a coverage check we already have.

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

1. `defgeneric` + single-ADT `defmethod` desugar to `match`; coverage
   errors; duplicate-variant errors. Reference: `examples/generics.cato`
   (shapes, both backends via the parity suite).
2. Whole-value and destructuring specializer forms.
3. `call-next-method` via hidden per-method `defn`s; reverse-declaration
   specificity; chained methods on one variant.
4. Two-parameter multiple dispatch with cross-product coverage checking.
5. (later) `T` fallback methods; method combination; jump tables.

Total new machinery: one desugar pass (form-level, like macro expansion),
one coverage checker reusing `match`'s, zero codegen changes for slices
1–2, zero for 3–4 beyond hidden defns. That is the argument for doing
generics this way: CLOS ergonomics, match economics.
