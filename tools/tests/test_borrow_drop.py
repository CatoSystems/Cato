import os
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
import borrow_drop


def run(src, *sources):
    """Rewrite `src`, with the extra sources contributing signatures."""
    sigs = {}
    for s in sources:
        borrow_drop.collect_signatures(borrow_drop._ac.parse_all(s), sigs)
    forms = borrow_drop._ac.parse_all(src)
    borrow_drop.collect_signatures(forms, sigs)
    edits = []
    for f in forms:
        borrow_drop.walk(f, sigs, edits)
    for start, end, repl in sorted(edits, reverse=True):
        src = src[:start] + repl + src[end:]
    return src


class TestBorrowDrop(unittest.TestCase):
    def test_borrowed_param_drops_the_clone(self):
        self.assertEqual(
            run("(defn g [t :- &Tab] :- i64 (size t))\n"
                "(defn f [t :- Tab] :- i64 (g (clone t)))"),
            "(defn g [t :- &Tab] :- i64 (size t))\n"
            "(defn f [t :- Tab] :- i64 (g t))")

    def test_owned_param_keeps_the_clone(self):
        # The callee consumes it, so the clone is load-bearing.
        self.assertEqual(
            run("(defn g [t :- Tab] :- i64 (size t))\n"
                "(defn f [t :- Tab] :- i64 (g (clone t)))"),
            "(defn g [t :- Tab] :- i64 (size t))\n"
            "(defn f [t :- Tab] :- i64 (g (clone t)))")

    def test_only_argument_positions_are_touched(self):
        self.assertEqual(
            run("(defn g [t :- &Tab] :- i64 (size t))\n"
                "(defn f [t :- Tab] :- Tab\n"
                "  (let [u (clone t)]\n"
                "    (do (g (clone t)) u)))"),
            "(defn g [t :- &Tab] :- i64 (size t))\n"
            "(defn f [t :- Tab] :- Tab\n"
                "  (let [u (clone t)]\n"
                "    (do (g t) u)))")

    def test_positional_match_per_argument(self):
        # Only the borrowed position loses its clone.
        self.assertEqual(
            run("(defn g [a :- Tab b :- &Tab] :- i64 (size a))\n"
                "(defn f [a :- Tab b :- Tab] :- i64 (g (clone a) (clone b)))"),
            "(defn g [a :- Tab b :- &Tab] :- i64 (size a))\n"
            "(defn f [a :- Tab b :- Tab] :- i64 (g (clone a) b))")

    def test_arity_mismatch_is_left_alone(self):
        self.assertEqual(
            run("(defn g [a :- &Tab] :- i64 (size a))\n"
                "(defn f [a :- Tab] :- i64 (g (clone a) 1))"),
            "(defn g [a :- &Tab] :- i64 (size a))\n"
            "(defn f [a :- Tab] :- i64 (g (clone a) 1))")

    def test_non_atom_argument_is_left_alone(self):
        self.assertEqual(
            run("(defn g [a :- &Tab] :- i64 (size a))\n"
                "(defn f [a :- Tab] :- i64 (g (clone (id a))))"),
            "(defn g [a :- &Tab] :- i64 (size a))\n"
            "(defn f [a :- Tab] :- i64 (g (clone (id a))))")

    def test_cross_file_signature(self):
        lib = "(defn g [t :- &Tab] :- i64 (size t))"
        self.assertEqual(run("(defn f [t :- Tab] :- i64 (g (clone t)))", lib),
                         "(defn f [t :- Tab] :- i64 (g t))")


if __name__ == "__main__":
    unittest.main()
