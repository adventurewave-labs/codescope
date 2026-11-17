package warehouse;

// Single in-repo implementation of the Stock interface.
public interface Stock {
    int pick(String sku, int qty);

    void putBack(String sku, int qty);
}
