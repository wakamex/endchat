/// endchat's mark, a line standing on a wall: the chat running down to its end, where every
/// turn works. Written by endchat's assets/icon/make_icon.py from the same rectangles as its SVG;
/// the two overlap and go the same way, so the winding fill joins them.
pub const HARNESS_ICON_ENDCHAT_POLY: &[Poly] = &[Poly {
    path: &[
        PolyCommand::MoveTo(BlockCoord::SquareFrac(55, 120), BlockCoord::SquareFrac(8, 120)),
        PolyCommand::LineTo(BlockCoord::SquareFrac(65, 120), BlockCoord::SquareFrac(8, 120)),
        PolyCommand::QuadTo { control: (BlockCoord::SquareFrac(69, 120), BlockCoord::SquareFrac(8, 120)), to: (BlockCoord::SquareFrac(69, 120), BlockCoord::SquareFrac(12, 120)) },
        PolyCommand::LineTo(BlockCoord::SquareFrac(69, 120), BlockCoord::SquareFrac(94, 120)),
        PolyCommand::QuadTo { control: (BlockCoord::SquareFrac(69, 120), BlockCoord::SquareFrac(98, 120)), to: (BlockCoord::SquareFrac(65, 120), BlockCoord::SquareFrac(98, 120)) },
        PolyCommand::LineTo(BlockCoord::SquareFrac(55, 120), BlockCoord::SquareFrac(98, 120)),
        PolyCommand::QuadTo { control: (BlockCoord::SquareFrac(51, 120), BlockCoord::SquareFrac(98, 120)), to: (BlockCoord::SquareFrac(51, 120), BlockCoord::SquareFrac(94, 120)) },
        PolyCommand::LineTo(BlockCoord::SquareFrac(51, 120), BlockCoord::SquareFrac(12, 120)),
        PolyCommand::QuadTo { control: (BlockCoord::SquareFrac(51, 120), BlockCoord::SquareFrac(8, 120)), to: (BlockCoord::SquareFrac(55, 120), BlockCoord::SquareFrac(8, 120)) },
        PolyCommand::Close,
        PolyCommand::MoveTo(BlockCoord::SquareFrac(14, 120), BlockCoord::SquareFrac(94, 120)),
        PolyCommand::LineTo(BlockCoord::SquareFrac(106, 120), BlockCoord::SquareFrac(94, 120)),
        PolyCommand::QuadTo { control: (BlockCoord::SquareFrac(110, 120), BlockCoord::SquareFrac(94, 120)), to: (BlockCoord::SquareFrac(110, 120), BlockCoord::SquareFrac(98, 120)) },
        PolyCommand::LineTo(BlockCoord::SquareFrac(110, 120), BlockCoord::SquareFrac(108, 120)),
        PolyCommand::QuadTo { control: (BlockCoord::SquareFrac(110, 120), BlockCoord::SquareFrac(112, 120)), to: (BlockCoord::SquareFrac(106, 120), BlockCoord::SquareFrac(112, 120)) },
        PolyCommand::LineTo(BlockCoord::SquareFrac(14, 120), BlockCoord::SquareFrac(112, 120)),
        PolyCommand::QuadTo { control: (BlockCoord::SquareFrac(10, 120), BlockCoord::SquareFrac(112, 120)), to: (BlockCoord::SquareFrac(10, 120), BlockCoord::SquareFrac(108, 120)) },
        PolyCommand::LineTo(BlockCoord::SquareFrac(10, 120), BlockCoord::SquareFrac(98, 120)),
        PolyCommand::QuadTo { control: (BlockCoord::SquareFrac(10, 120), BlockCoord::SquareFrac(94, 120)), to: (BlockCoord::SquareFrac(14, 120), BlockCoord::SquareFrac(94, 120)) },
        PolyCommand::Close,
    ],
    intensity: BlockAlpha::Full,
    style: PolyStyle::Fill,
}];
