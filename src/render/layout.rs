// Layout computation: converts the parsed element tree into positioned elements using taffy.

use anyhow::Result;
use taffy::prelude::*;

use super::parser::{Element, ElementStyle};

/// A positioned, renderable node (output of layout computation).
#[derive(Debug, Clone)]
pub struct LayoutNode {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub text: Option<String>,
    pub style: ElementStyle,
}

/// Convert our ElementStyle to a taffy Style.
fn to_taffy_style(es: &ElementStyle) -> Style {
    let mut style = Style::default();

    match es.display.as_deref() {
        Some("flex") => style.display = Display::Flex,
        _ => style.display = Display::Flex, // Default to flex
    }

    match es.flex_direction.as_deref() {
        Some("column") => style.flex_direction = FlexDirection::Column,
        Some("row-reverse") => style.flex_direction = FlexDirection::RowReverse,
        Some("column-reverse") => style.flex_direction = FlexDirection::ColumnReverse,
        _ => style.flex_direction = FlexDirection::Row,
    }

    match es.justify_content.as_deref() {
        Some("center") => style.justify_content = Some(JustifyContent::Center),
        Some("space-between") => style.justify_content = Some(JustifyContent::SpaceBetween),
        Some("space-around") => style.justify_content = Some(JustifyContent::SpaceAround),
        Some("flex-end") => style.justify_content = Some(JustifyContent::FlexEnd),
        _ => {}
    }

    match es.align_items.as_deref() {
        Some("center") => style.align_items = Some(AlignItems::Center),
        Some("flex-start") => style.align_items = Some(AlignItems::FlexStart),
        Some("flex-end") => style.align_items = Some(AlignItems::FlexEnd),
        Some("stretch") => style.align_items = Some(AlignItems::Stretch),
        _ => {}
    }

    if let Some(gap) = es.gap {
        style.gap = Size { width: length(gap), height: length(gap) };
    }

    if let Some(p) = es.padding {
        let lp = LengthPercentage::Length(p);
        style.padding = Rect { left: lp, right: lp, top: lp, bottom: lp };
    }

    if let Some(m) = es.margin {
        let lpa = LengthPercentageAuto::Length(m);
        style.margin = Rect { left: lpa, right: lpa, top: lpa, bottom: lpa };
    }

    if let Some(w) = es.width {
        style.size.width = length(w);
    }
    if let Some(h) = es.height {
        style.size.height = length(h);
    }

    style
}

/// Recursively build taffy nodes from our element tree.
fn build_taffy_tree(
    taffy: &mut TaffyTree<usize>,
    element: &Element,
    nodes_out: &mut Vec<(NodeId, Element)>,
) -> Result<NodeId> {
    let taffy_style = to_taffy_style(&element.style);

    if element.children.is_empty() {
        // Leaf node
        let node = taffy.new_leaf(taffy_style)?;
        nodes_out.push((node, element.clone()));
        Ok(node)
    } else {
        // Container with children — process children first, then create container
        let mut child_ids = Vec::new();
        for child in &element.children {
            let child_id = build_taffy_tree(taffy, child, nodes_out)?;
            child_ids.push(child_id);
        }
        let node = taffy.new_with_children(taffy_style, &child_ids)?;
        nodes_out.push((node, element.clone()));
        Ok(node)
    }
}

/// Compute layout for an element tree. Returns flat list of positioned nodes.
pub fn compute_layout(root: &Element, container_w: f32, container_h: f32) -> Result<Vec<LayoutNode>> {
    let mut taffy: TaffyTree<usize> = TaffyTree::new();
    let mut node_map: Vec<(NodeId, Element)> = Vec::new();

    let root_id = build_taffy_tree(&mut taffy, root, &mut node_map)?;

    taffy.compute_layout(root_id, Size {
        width: AvailableSpace::Definite(container_w),
        height: AvailableSpace::Definite(container_h),
    })?;

    // Collect layout results with absolute positions
    let mut result = Vec::new();
    collect_layout_nodes(&taffy, root_id, 0.0, 0.0, &node_map, &mut result)?;
    Ok(result)
}

fn collect_layout_nodes(
    taffy: &TaffyTree<usize>,
    node_id: NodeId,
    parent_x: f32,
    parent_y: f32,
    node_map: &[(NodeId, Element)],
    out: &mut Vec<LayoutNode>,
) -> Result<()> {
    let layout = taffy.layout(node_id)?;
    let abs_x = parent_x + layout.location.x;
    let abs_y = parent_y + layout.location.y;

    if let Some((_, element)) = node_map.iter().find(|(id, _)| *id == node_id) {
        out.push(LayoutNode {
            x: abs_x,
            y: abs_y,
            width: layout.size.width,
            height: layout.size.height,
            text: element.text.clone(),
            style: element.style.clone(),
        });
    }

    for &child_id in taffy.children(node_id)?.iter() {
        collect_layout_nodes(taffy, child_id, abs_x, abs_y, node_map, out)?;
    }
    Ok(())
}
