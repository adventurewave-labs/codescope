package app;

import java.util.List;

public class Main {
    public static void main(String[] args) {
        Repo r = new Repo(); // @eval Repo=Repo
        r.save("x"); // @eval save=Repo::save
        Cache c = new Cache(); // @eval Cache=Cache
        c.save("y"); // @eval save=Cache::save
        System.out.println("z"); // @eval println=-
        run(); // @eval run=Main::run
        String.valueOf(1); // @eval valueOf=-
    }
    static void run() {}
}
