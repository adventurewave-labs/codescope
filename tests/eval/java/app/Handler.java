package app;

public class Handler {
    private final Repo repo = new Repo(); // @eval Repo=Repo

    void handle() {
        repo.save("x"); // @eval save=Repo::save
        this.repo.save("y"); // @eval save=Repo::save
    }
}
