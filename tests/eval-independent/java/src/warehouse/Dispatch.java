package warehouse;

import java.util.List;

public class Dispatch {
    private final Stock stock;

    public Dispatch(Stock stock) {
        this.stock = stock;
    }

    public String fulfill(String sku, int qty) {
        int taken = stock.pick(sku, qty); // @eval pick=LocalStock::pick@src/warehouse/LocalStock.java
        LocalDate day = LocalDate.now(); // @eval now=-
        return label(sku, taken, day); // @eval label=Dispatch::label@src/warehouse/Dispatch.java
    }

    static String label(String sku, int qty, LocalDate day) {
        return String.format("%s x%d @ %s", sku, qty, String.valueOf(day)); // @eval valueOf=-
    }
}
