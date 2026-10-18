package main; import "fmt"; func main() { s:="hello world benchmark string"; sum:=0; for i:=0;i<len(s);i++{sum+=int(s[i])}; fmt.Println(sum) }
