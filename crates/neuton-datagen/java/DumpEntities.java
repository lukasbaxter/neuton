// The name and size of every entity type.
//
// Sizes are not in the data reports and they are not cosmetic: the box is what
// a hit has to land in, so a client that guesses one guesses the reach it gives
// the player. Registry order is the wire order, so the index here is the id the
// add-entity packet sends.

import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.SharedConstants;
import net.minecraft.world.entity.EntityType;
import java.nio.file.Files;
import java.nio.file.Path;

public final class DumpEntities {
    public static void main(String[] args) throws Exception {
        Path out_path = Path.of(args[0]);
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        StringBuilder out = new StringBuilder();
        out.append("[\n");
        boolean first = true;
        for (EntityType<?> type : BuiltInRegistries.ENTITY_TYPE) {
            if (!first) out.append(",\n");
            first = false;
            out.append("{\"id\":").append(BuiltInRegistries.ENTITY_TYPE.getId(type))
               .append(",\"name\":\"")
               .append(BuiltInRegistries.ENTITY_TYPE.getKey(type))
               .append("\",\"width\":").append(type.getDimensions().width())
               .append(",\"height\":").append(type.getDimensions().height())
               .append(",\"eye\":").append(type.getDimensions().eyeHeight())
               .append("}");
        }
        out.append("\n]\n");
        Files.writeString(out_path, out.toString());
    }
}
