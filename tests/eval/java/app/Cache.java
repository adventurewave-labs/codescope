package app;

public class Cache {
    public void save(String s) {
        this.validate(s); // @eval validate=Cache::validate
    }
    void validate(String s) {}
}
