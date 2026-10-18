package main; import "fmt"; func main() { v:=make([]int,0); for i:=0;i<2000;i++{v=append(v,i)}; fmt.Println(len(v)) }
