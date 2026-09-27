package app;

public class Repo {
    public void save(String s) {
        validate(s); // @eval validate=Repo::validate
        items.add(s); // @eval add=-
    }
    void validate(String s) {}
}
