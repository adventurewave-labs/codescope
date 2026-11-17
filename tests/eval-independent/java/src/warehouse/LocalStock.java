package warehouse;

import java.time.LocalDate;
import java.util.HashMap;
import java.util.Map;

public class LocalStock implements Stock {
    private final Map<String, Integer> counts = new HashMap<>();

    @Override
    public int pick(String sku, int qty) {
        requireIntegrity(sku); // @eval requireIntegrity=LocalStock::requireIntegrity@src/warehouse/LocalStock.java
        int have = counts.getOrDefault(sku, 0);
        counts.put(sku, Math.max(0, have - qty));
        return have;
    }

    @Override
    public void putBack(String sku, int qty) {
        counts.merge(sku, qty, Integer::sum); // @eval merge=-
    }

    private static void requireIntegrity(String sku) {
        if (sku == null || sku.isBlank()) { // @eval isBlank=-
            throw new IllegalArgumentException("bad sku");
        }
    }

    private static void merge(String sku, int qty) {
        // unreachable decoy: merge is shadowed by Map.merge usage above;
        // LocalStock.putBack calls Map.merge, NOT this method.
    }
}
