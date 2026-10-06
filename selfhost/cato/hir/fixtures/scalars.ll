; cato: kernel LLVM IR (S7)
%VecI64 = type { ptr, i64, i64 }
%Closure = type { ptr, ptr, ptr }
%Option = type { i64, [1 x i64] }
declare void @cato_spit(ptr, ptr)
declare ptr @cato_slurp(ptr)
declare void @cato_print_str_raw(ptr)
declare void @cato_print_string(ptr)
declare void @cato_print_i64(i64)
declare void @cato_print_f64(double)
declare i32 @strcmp(ptr, ptr)
declare i64 @strlen(ptr)
declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1, i1)
declare void @cato_str_vec_free(ptr, i64)
declare ptr @cato_str_vec_copy(ptr, i64, i64)
declare ptr @strdup(ptr)
declare void @free(ptr)
declare ptr @malloc(i64)
declare ptr @cato_args(ptr)
declare void @cato_args_init(i32, ptr)

define i64 @scal(i64 %x, i64 %y) {
entry:
  %t0 = mul i64 %y, 2
  %t1 = add i64 %x, %t0
  %t2 = sub i64 %t1, %y
  %t3 = sdiv i64 %t2, 2
  ret i64 %t3

}


define i64 @widths(i64 %x) {
entry:
  %t0 = trunc i64 %x to i8
  %t1 = trunc i64 %x to i32
  %t2 = sext i8 %t0 to i64
  %t3 = add i64 %t2, 1
  %t4 = sext i32 %t1 to i64
  %t5 = add i64 %t4, 1
  %t6 = zext i8 %t0 to i64
  %t7 = add i64 %t5, %t6
  %t8 = add i64 %t3, %t7
  ret i64 %t8

}


define double @floats(double %x) {
entry:
  %t0 = fmul double %x, 2.0
  %t1 = fadd double %t0, 0.5
  %t2 = fdiv double %t1, 2.0
  %t3 = fptrunc double %t1 to float
  %t4 = fpext float %t3 to double
  %t5 = fadd double %t2, %t4
  ret double %t5

}


define i64 @cmps(i64 %x, i64 %y) {
entry:
  %t0 = icmp slt i64 %x, %y
  %t1 = icmp sge i64 %x, %y
  %t2 = icmp eq i64 %x, %y
  %t3 = icmp ne i64 %x, %y
  %t4 = icmp sgt i64 %x, %y
  %t5 = select i1 %t4, i64 %x, i64 %y
  %t6 = icmp slt i64 %x, %y
  %t7 = select i1 %t6, i64 %x, i64 %y
  %t8 = sub i64 %x, %y
  %t9 = sub i64 0, %t8
  %t10 = icmp slt i64 %t8, 0
  %t11 = select i1 %t10, i64 %t9, i64 %t8
  br i1 %t0, label %b0, label %b1
b0:
  br i1 %t1, label %b3, label %b2
b2:
  br label %b3
b3:
  %t12 = phi i1 [ true, %b0 ], [ %t2, %b2 ]
  br i1 %t12, label %b4, label %b5
b4:
  br label %b5
b5:
  %t13 = phi i1 [ false, %b3 ], [ %t3, %b4 ]
  br label %b1
b1:
  %t14 = phi i1 [ false, %entry ], [ %t13, %b5 ]
  br i1 %t14, label %b6, label %b7
b6:
  %t15 = add i64 %t7, %t11
  %t16 = add i64 %t5, %t15
  br label %b8
b7:
  br label %b8
b8:
  %t17 = phi i64 [ %t16, %b6 ], [ 0, %b7 ]
  ret i64 %t17

}


define i32 @main(i32 %argc, ptr %argv) {
entry:
  call void @cato_args_init(i32 %argc, ptr %argv)
  %t0 = call i64 @scal(i64 3, i64 4)
  %t1 = call i64 @widths(i64 100)
  %t2 = call i64 @cmps(i64 5, i64 9)
  %t3 = add i64 %t1, %t2
  %t4 = add i64 %t0, %t3

  ret i32 0
}



