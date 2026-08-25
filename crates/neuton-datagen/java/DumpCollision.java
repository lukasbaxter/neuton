// Dumps the box a player actually walks into, per block state.
//
// The render model is not that box. The game keeps collision separate, in
// code, and for a fence or a wall it is half a block taller than anything
// drawn, so that a fence cannot be jumped. Deriving collision from the model
// gets those wrong in both directions at once: too short to stop a jump, and
// the wrong footprint to walk past.
//
// Written to a file rather than stdout, because the game logs on the way up.

import net.minecraft.core.BlockPos;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.SharedConstants;
import net.minecraft.world.level.EmptyBlockGetter;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.phys.AABB;
import net.minecraft.world.phys.shapes.VoxelShape;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;

public final class DumpCollision {
    // What a player walks into.
    private static VoxelShape collisionOf(BlockState state) {
        try {
            return state.getCollisionShape(EmptyBlockGetter.INSTANCE, BlockPos.ZERO);
        } catch (Exception e) {
            // A few blocks want a real level to answer. Empty means "walk
            // through it", which is the safer of the two wrong answers.
            return null;
        }
    }

    // What the crosshair picks and what the selection box is drawn around.
    // Not the same as collision: a fence is drawn one block tall and walked
    // into one and a half.
    private static VoxelShape outlineOf(BlockState state) {
        try {
            return state.getShape(EmptyBlockGetter.INSTANCE, BlockPos.ZERO);
        } catch (Exception e) {
            return null;
        }
    }

    private static void appendShape(StringBuilder out, VoxelShape shape) {
        out.append("[");
        if (shape != null) {
            List<AABB> boxes = shape.toAabbs();
            for (int i = 0; i < boxes.size(); i++) {
                AABB b = boxes.get(i);
                if (i > 0) out.append(",");
                out.append("[")
                   .append(b.minX).append(",").append(b.minY).append(",").append(b.minZ)
                   .append(",")
                   .append(b.maxX).append(",").append(b.maxY).append(",").append(b.maxZ)
                   .append("]");
            }
        }
        out.append("]");
    }

    public static void main(String[] args) throws Exception {
        Path out_path = Path.of(args[0]);
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        StringBuilder out = new StringBuilder();
        out.append("[\n");
        boolean first = true;
        for (Block block : BuiltInRegistries.BLOCK) {
            for (BlockState state : block.getStateDefinition().getPossibleStates()) {
                if (!first) out.append(",\n");
                first = false;
                int id = Block.getId(state);
                out.append("{\"id\":").append(id).append(",\"collision\":");
                appendShape(out, collisionOf(state));
                out.append(",\"outline\":");
                appendShape(out, outlineOf(state));
                out.append("}");
            }
        }
        out.append("\n]\n");
        Files.writeString(out_path, out.toString());
    }
}
