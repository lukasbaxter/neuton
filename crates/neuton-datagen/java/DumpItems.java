// Dumps what a client needs to predict a click on an inventory slot.
//
// How many of a thing fit in one slot, and which armour slot it goes in, are
// both components on the item's default stack rather than anything in the data
// reports. A client that guesses instead gets the common cases wrong -- a
// stack of ender pearls is sixteen, a tool is one -- and every wrong guess is
// a slot that shows the wrong thing until the server corrects it.

import net.minecraft.core.HolderLookup;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.core.component.DataComponentInitializers;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.core.component.DataComponentMap;
import net.minecraft.core.component.DataComponents;
import net.minecraft.resources.Identifier;
import net.minecraft.server.Bootstrap;
import net.minecraft.SharedConstants;
import net.minecraft.world.entity.EquipmentSlot;
import net.minecraft.world.item.Item;
import net.minecraft.world.item.equipment.Equippable;
import java.nio.file.Files;
import java.nio.file.Path;

public final class DumpItems {
    public static void main(String[] args) throws Exception {
        // To a file rather than stdout: the game logs a line of its own on the
        // way up and mixing that into the output is a parsing problem nobody
        // needs.
        Path outPath = Path.of(args[0]);
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        // 26.x builds an item's default components from initializers rather
        // than storing them on the item, and nothing binds them until a
        // registry lookup exists to run them against. Asking an unbound item
        // what it holds throws "Components not bound yet"; this is the same
        // lookup the vanilla data generator builds.
        HolderLookup.Provider lookup = VanillaRegistries.createLookup();
        for (DataComponentInitializers.PendingComponents<?> pending :
                BuiltInRegistries.DATA_COMPONENT_INITIALIZERS.build(lookup)) {
            pending.apply();
        }

        StringBuilder out = new StringBuilder();
        out.append("[\n");
        boolean first = true;
        for (Item item : BuiltInRegistries.ITEM) {
            Identifier id = BuiltInRegistries.ITEM.getKey(item);
            // The item's own default components rather than a stack of it:
            // building a stack asks the registry holder for components that
            // are only bound once a world has loaded, and there is no world
            // here.
            DataComponentMap components = item.components();
            Integer max = components.get(DataComponents.MAX_STACK_SIZE);
            String slot = "";
            Equippable equippable = components.get(DataComponents.EQUIPPABLE);
            if (equippable != null) {
                EquipmentSlot where = equippable.slot();
                slot = where.getName();
            }
            boolean damageable = components.has(DataComponents.MAX_DAMAGE);
            if (!first) out.append(",\n");
            first = false;
            out.append("{\"name\":\"").append(id.getPath()).append("\"")
               .append(",\"max\":").append(max == null ? 64 : max)
               .append(",\"slot\":\"").append(slot).append("\"")
               .append(",\"damageable\":").append(damageable)
               .append("}");
        }
        out.append("\n]\n");
        Files.writeString(outPath, out.toString());
    }
}
