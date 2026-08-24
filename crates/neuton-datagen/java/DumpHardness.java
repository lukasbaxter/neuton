// Dumps what a client needs to predict how long a block takes to break.
//
// None of this is in the vanilla data reports: hardness lives in code, and the
// tool a block wants is a tag. Both are one call away with the jar on the
// classpath, which since 26.x is unobfuscated.

import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.SharedConstants;
import net.minecraft.tags.BlockTags;
import net.minecraft.core.BlockPos;
import net.minecraft.world.level.EmptyBlockGetter;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;

public final class DumpHardness {
    public static void main(String[] args) {
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
                float hardness = state.getDestroySpeed(EmptyBlockGetter.INSTANCE, BlockPos.ZERO);
                boolean needsTool = state.requiresCorrectToolForDrops();
                String tool = state.is(BlockTags.MINEABLE_WITH_PICKAXE) ? "pickaxe"
                        : state.is(BlockTags.MINEABLE_WITH_AXE) ? "axe"
                        : state.is(BlockTags.MINEABLE_WITH_SHOVEL) ? "shovel"
                        : state.is(BlockTags.MINEABLE_WITH_HOE) ? "hoe"
                        : "";
                out.append("{\"id\":").append(id)
                   .append(",\"hardness\":").append(hardness)
                   .append(",\"needs_tool\":").append(needsTool)
                   .append(",\"tool\":\"").append(tool).append("\"}");
            }
        }
        out.append("\n]\n");
        System.out.println(out);
    }
}
