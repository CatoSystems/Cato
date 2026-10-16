import os
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
import unclone


def run(src):
    return unclone.unclone_text(src, [])[0]


class TestUnclone(unittest.TestCase):
    def test_single_use_becomes_move(self):
        self.assertEqual(
            run("(defn f [x :- Box] :- i64 (get (clone x)))"),
            "(defn f [x :- Box] :- i64 (get x))")

    def test_last_of_two_uses_moves_earlier_stays_cloned(self):
        self.assertEqual(
            run("(defn f [x :- Box] :- i64 (+ (size (clone x)) (size (clone x))))"),
            "(defn f [x :- Box] :- i64 (+ (size (clone x)) (size x)))")

    def test_match_arm_binders_move_per_arm(self):
        src = ("(defn f [e :- HExpr] :- HExpr\n"
               "  (match e\n"
               "    (HLit [v] (HLit (clone v)))\n"
               "    (HOther [n] (HOther (clone n)))))")
        out = run(src)
        self.assertIn("(HLit v)", out)
        self.assertIn("(HOther n)", out)

    def test_same_binder_moves_in_each_arm(self):
        # Arms are alternative control paths: the SAME binder may move in each.
        src = ("(defn f [e :- E] :- E\n"
               "  (match e\n"
               "    (A [x] (clone e))\n"
               "    (B [y] (clone e))))")
        out = run(src)
        self.assertNotIn("(clone e)", out)

    def test_shadowed_inner_binding_not_counted(self):
        src = ("(defn f [x :- Box] :- i64\n"
               "  (let [y (size (clone x))]\n"
               "    (+ y (size (clone x)))))")
        out = run(src)
        self.assertIn("(size x)", out)          # last use on the path moves
        self.assertIn("(size (clone x))", out)  # the earlier use stays cloned

    def test_strings_and_comments_untouched(self):
        src = ('(defn f [x :- Box] :- str\n'
               '  ; (clone x) in a comment\n'
               '  (get (str (clone x))))')
        out = run(src)
        self.assertIn("; (clone x) in a comment", out)

    def test_copy_types_left_alone(self):
        src = "(defn f [n :- i64] :- i64 (add (clone n) (clone n)))"
        self.assertEqual(run(src), src)

    def test_idempotent(self):
        src = "(defn f [x :- Box] :- i64 (get (clone x)))"
        self.assertEqual(run(run(src)), run(src))


if __name__ == "__main__":
    unittest.main()
