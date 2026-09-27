package main

import (
	"fmt"
	"example.com/app/store"
)

type Server struct{}

func (srv *Server) Start() {
	srv.listen() // @eval listen=Server::listen
}

func (srv *Server) listen() {}

func main() {
	s := store.NewStore() // @eval NewStore=NewStore
	s.Put("k") // @eval Put=Store::Put
	store.Open() // @eval Open=Open
	fmt.Println("x") // @eval Println=-
	helper() // @eval helper=helper
}

func helper() {}

type App struct {
	st *store.Store
}

func (a *App) Save() {
	a.st.Put("k") // @eval Put=Store::Put
}
