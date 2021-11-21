use crate::geometry;
use crate::strokes::strokestyle::{Element, StrokeBehaviour};
use crate::{
    compose, curves,
    pens::brush::{self, Brush},
    render,
};

use gtk4::gsk;
use p2d::bounding_volume::BoundingVolume;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use svg::node::element::path;

// Struct field names are also used in brushstroke template, reminder to be careful when renaming
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TeraElement {
    // Pressure from 0.0 to 1.0
    pressure: f64,
    // Position in format `x y` as integer values
    x: f64,
    y: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BrushStroke {
    pub elements: Vec<Element>,
    pub brush: Brush,
    pub bounds: p2d::bounding_volume::AABB,
    #[serde(skip)]
    pub hitbox: Vec<p2d::bounding_volume::AABB>,
}

impl Default for BrushStroke {
    fn default() -> Self {
        Self::new(Element::default(), Brush::default())
    }
}

impl StrokeBehaviour for BrushStroke {
    fn bounds(&self) -> p2d::bounding_volume::AABB {
        self.bounds
    }

    fn translate(&mut self, offset: na::Vector2<f64>) {
        let new_elements: Vec<Element> = self
            .elements
            .iter()
            .map(|element| {
                let mut new_element = element.clone();
                new_element
                    .inputdata
                    .set_pos(element.inputdata.pos() + offset);
                new_element
            })
            .collect();

        self.elements = new_elements;
        self.update_bounds();
        self.hitbox = self.gen_hitbox();
    }

    fn resize(&mut self, new_bounds: p2d::bounding_volume::AABB) {
        let offset = na::vector![
            new_bounds.mins[0] - self.bounds.mins[0],
            new_bounds.mins[1] - self.bounds.mins[1]
        ];

        let scalevector = na::vector![
            (new_bounds.maxs[0] - new_bounds.mins[0]) / (self.bounds.maxs[0] - self.bounds.mins[0]),
            (new_bounds.maxs[1] - new_bounds.mins[1]) / (self.bounds.maxs[1] - self.bounds.mins[1])
        ];

        let new_elements: Vec<Element> = self
            .elements
            .iter()
            .map(|element| {
                let mut new_element = element.clone();
                let top_left = na::vector![self.bounds.mins[0], self.bounds.mins[1]];

                new_element.inputdata.set_pos(
                    ((element.inputdata.pos() - top_left).component_mul(&scalevector))
                        + top_left
                        + offset,
                );

                new_element
            })
            .collect();

        self.elements = new_elements;
        self.bounds = new_bounds;
        self.hitbox = self.gen_hitbox();
    }

    fn gen_svg_data(&self, offset: na::Vector2<f64>) -> Result<String, anyhow::Error> {
        match self.brush.current_style {
            brush::BrushStyle::Linear => Ok(compose::wrap_svg(
                &self
                    .linear_svg_data(offset, false)?
                    .iter()
                    .map(|svg| &svg.svg_data)
                    .fold(String::from(""), |first, second| {
                        first + "\n" + second.as_str()
                    }),
                Some(self.bounds),
                Some(self.bounds),
                true,
                false,
            )),
            brush::BrushStyle::CubicBezier => Ok(compose::wrap_svg(
                &self
                    .cubbez_svg_data(offset, false)?
                    .iter()
                    .map(|svg| &svg.svg_data)
                    .fold(String::from(""), |first, second| {
                        first + "\n" + second.as_str()
                    }),
                Some(self.bounds),
                Some(self.bounds),
                true,
                false,
            )),
            brush::BrushStyle::CustomTemplate(_) => {
                if let Some(template_svg) = self.templates_svg_data(offset)? {
                    Ok(compose::wrap_svg(
                        template_svg.svg_data.as_str(),
                        Some(self.bounds),
                        Some(self.bounds),
                        true,
                        false,
                    ))
                } else {
                    Ok(String::from(""))
                }
            }
            brush::BrushStyle::Experimental => {
                if let Some(experimental_svg) = self.experimental_svg_data(offset)? {
                    Ok(compose::wrap_svg(
                        experimental_svg.svg_data.as_str(),
                        Some(self.bounds),
                        Some(self.bounds),
                        true,
                        false,
                    ))
                } else {
                    Ok(String::from(""))
                }
            }
        }
    }

    fn gen_rendernode(
        &self,
        scalefactor: f64,
        renderer: &render::Renderer,
    ) -> Result<Option<gsk::RenderNode>, anyhow::Error> {
        let offset = na::vector![0.0, 0.0];
        let svgs: Vec<render::Svg> = match self.brush.current_style {
            brush::BrushStyle::Linear => self.linear_svg_data(offset, true)?,
            brush::BrushStyle::CubicBezier => self.cubbez_svg_data(offset, true)?,
            /*brush::BrushStyle::CustomTemplate(_) => vec![self.templates_svg_data(offset)?],
            brush::BrushStyle::Experimental => vec![self.experimental_svg_data(offset)?], */
            _ => {
                vec![]
            }
        };

        Ok(renderer
            .gen_rendernode_par(scalefactor, &svgs)
            .map_err(|e| {
                anyhow::anyhow!(
                    "gen_rendernode_par() failed in gen_rendernode() for brushstroke, {}",
                    e
                )
            })?)
    }
}

impl BrushStroke {
    pub const HITBOX_DEFAULT: f64 = 10.0;

    pub fn new(element: Element, brush: Brush) -> Self {
        let elements = Vec::with_capacity(20);
        let bounds = p2d::bounding_volume::AABB::new(
            na::point![element.inputdata.pos()[0], element.inputdata.pos()[1]],
            na::point![element.inputdata.pos()[0], element.inputdata.pos()[1]],
        );
        let hitbox = Vec::new();

        let mut brushstroke = Self {
            elements,
            brush,
            bounds,
            hitbox,
        };

        // Pushing with push_elem() instead filling vector, because bounds are getting updated there too
        brushstroke.push_elem(element);

        brushstroke
    }

    pub fn validation_stroke(elements: &[Element], brush: &Brush) -> Option<Self> {
        let mut data_entries_iter = elements.iter();
        let mut stroke = if let Some(first_entry) = data_entries_iter.next() {
            Self::new(first_entry.clone(), brush.clone())
        } else {
            return None;
        };

        for data_entry in data_entries_iter {
            stroke.push_elem(data_entry.clone());
        }
        stroke.complete_stroke();

        Some(stroke)
    }

    pub fn push_elem(&mut self, element: Element) {
        self.elements.push(element);

        self.update_bounds_to_last_elem();
    }

    pub fn pop_elem(&mut self) -> Option<Element> {
        let element = self.elements.pop();

        self.complete_stroke();

        element
    }

    pub fn complete_stroke(&mut self) {
        self.update_bounds();
        self.hitbox = self.gen_hitbox();
    }

    fn update_bounds_to_last_elem(&mut self) {
        // Making sure bounds are always outside of coord + width
        if let Some(last) = self.elements.last() {
            self.bounds.merge(&p2d::bounding_volume::AABB::new(
                na::Point2::from(
                    last.inputdata.pos() - na::vector![self.brush.width(), self.brush.width()],
                ),
                na::Point2::from(
                    last.inputdata.pos() + na::vector![self.brush.width(), self.brush.width()],
                ),
            ));
        }
    }

    fn update_bounds(&mut self) {
        let mut elements_iter = self.elements.iter();
        if let Some(first) = elements_iter.next() {
            self.bounds = p2d::bounding_volume::AABB::new_invalid();

            self.bounds.merge(&p2d::bounding_volume::AABB::new(
                na::Point2::from(
                    first.inputdata.pos() - na::vector![self.brush.width(), self.brush.width()],
                ),
                na::Point2::from(
                    first.inputdata.pos() + na::vector![self.brush.width(), self.brush.width()],
                ),
            ));
            for element in elements_iter {
                self.bounds.merge(&p2d::bounding_volume::AABB::new(
                    na::Point2::from(
                        element.inputdata.pos()
                            - na::vector![self.brush.width(), self.brush.width()],
                    ),
                    na::Point2::from(
                        element.inputdata.pos()
                            + na::vector![self.brush.width(), self.brush.width()],
                    ),
                ));
            }
        }
    }

    fn gen_hitbox(&self) -> Vec<p2d::bounding_volume::AABB> {
        let mut hitbox: Vec<p2d::bounding_volume::AABB> =
            Vec::with_capacity(self.elements.len() as usize);
        let mut elements_iter = self.elements.iter().peekable();
        while let Some(first) = elements_iter.next() {
            let second = if let Some(&second) = elements_iter.peek() {
                Some(second)
            } else {
                None
            };
            hitbox.push(self.gen_last_hitbox(first, second));
        }

        hitbox
    }

    fn gen_last_hitbox(
        &self,
        first: &Element,
        second: Option<&Element>,
    ) -> p2d::bounding_volume::AABB {
        let brush_width = self.brush.width();

        let first = first.inputdata.pos();
        if let Some(second) = second {
            let second = second.inputdata.pos();

            let delta = second - first;
            let brush_x = if delta[0] < 0.0 {
                -brush_width
            } else {
                brush_width
            };
            let brush_y = if delta[1] < 0.0 {
                -brush_width
            } else {
                brush_width
            };

            geometry::aabb_new_positive(
                first - na::vector![brush_x / 2.0, brush_y / 2.0],
                first + delta + na::vector![brush_x / 2.0, brush_y / 2.0],
            )
        } else {
            geometry::aabb_new_positive(
                first
                    - na::vector![
                        (Self::HITBOX_DEFAULT + brush_width) / 2.0,
                        (Self::HITBOX_DEFAULT + brush_width / 2.0)
                    ],
                first
                    + na::vector![
                        Self::HITBOX_DEFAULT + brush_width,
                        Self::HITBOX_DEFAULT + brush_width
                    ],
            )
        }
    }

    pub fn linear_svg_data(
        &self,
        offset: na::Vector2<f64>,
        svg_root: bool,
    ) -> Result<Vec<render::Svg>, anyhow::Error> {
        if self.elements.len() <= 1 {
            return Ok(vec![]);
        }

        let commands: Vec<render::Svg> = self
            .elements
            .par_iter()
            .zip(self.elements.par_iter().skip(1))
            .zip(self.elements.par_iter().skip(2))
            .zip(self.elements.par_iter().skip(3))
            .enumerate()
            .filter_map(|(i, (((first, second), third), forth))| {
                let mut commands = Vec::new();

                let start_width = second.inputdata.pressure() * self.brush.width();
                let end_width = third.inputdata.pressure() * self.brush.width();

                let mut bounds = p2d::bounding_volume::AABB::new_invalid();

                if let Some(mut cubbez) =
                    curves::gen_cubbez_w_catmull_rom(first, second, third, forth)
                {
                    cubbez.start += offset;
                    cubbez.cp1 += offset;
                    cubbez.cp2 += offset;
                    cubbez.end += offset;

                    // Bounds are definitely inside the polygon of the control points. (Could be improved with the second derivative of the bezier curve)
                    bounds.take_point(na::Point2::<f64>::from(cubbez.start));
                    bounds.take_point(na::Point2::<f64>::from(cubbez.cp1));
                    bounds.take_point(na::Point2::<f64>::from(cubbez.cp2));
                    bounds.take_point(na::Point2::<f64>::from(cubbez.end));

                    bounds.loosen(start_width.max(end_width));
                    // Ceil to nearest integers to avoid subpixel placement errors between stroke elements.
                    bounds = geometry::aabb_ceil(bounds);

                    // Number of splits for the bezier curve approximation
                    let n_splits = 7;
                    for (i, line) in curves::approx_cubbez_with_lines(cubbez, n_splits)
                        .iter()
                        .enumerate()
                    {
                        // splitted line start / end widths are a linear interpolation between the start and end width / n splits
                        let line_start_width = start_width
                            + (end_width - start_width)
                                * (f64::from(i as i32) / f64::from(n_splits));
                        let line_end_width = start_width
                            + (end_width - start_width)
                                * (f64::from(i as i32 + 1) / f64::from(n_splits));

                        commands.append(&mut compose::compose_linear_variable_width(
                            *line,
                            line_start_width,
                            line_end_width,
                            true,
                        ));
                    }
                } else if let Some(mut line) = curves::gen_line(second, third) {
                    line.start += offset;
                    line.end += offset;

                    if i == 0 {
                        commands.push(path::Command::Move(
                            path::Position::Absolute,
                            path::Parameters::from((line.start[0], line.start[1])),
                        ));
                    }
                    commands.append(&mut compose::compose_linear_variable_width(
                        line,
                        start_width,
                        end_width,
                        true,
                    ));
                }

                let path = svg::node::element::Path::new()
                    .set("stroke", "none")
                    //.set("stroke", self.brush.color.to_css_color())
                    //.set("stroke-width", 1.0)
                    .set("fill", self.brush.color.to_css_color())
                    .set("d", path::Data::from(commands));

                match rough_rs::node_to_string(&path) {
                    Ok(mut svg_data) => {
                        if svg_root {

                        svg_data =
                            compose::wrap_svg(&svg_data, Some(bounds), Some(bounds), true, false);
                        }
                        Some(render::Svg { svg_data, bounds })
                    }
                    Err(e) => {
                        log::error!("rough_rs::node_to_string() failed in linear_svg_data() of brushstroke, {}", e);
                        None
                    }
                }
            })
            .collect();

        Ok(commands)
    }

    pub fn cubbez_svg_data(
        &self,
        offset: na::Vector2<f64>,
        svg_root: bool,
    ) -> Result<Vec<render::Svg>, anyhow::Error> {
        if self.elements.len() <= 1 {
            return Ok(vec![]);
        }

        let svgs: Vec<render::Svg> = self
            .elements
            .par_iter()
            .zip(self.elements.par_iter().skip(1))
            .zip(self.elements.par_iter().skip(2))
            .zip(self.elements.par_iter().skip(3))
            .filter_map(|(((first, second), third), forth)| {
                let mut commands = Vec::new();
                let start_width = second.inputdata.pressure() * self.brush.width();
                let end_width = third.inputdata.pressure() * self.brush.width();

                let mut bounds = p2d::bounding_volume::AABB::new_invalid();

                if let Some(mut cubbez) =
                    curves::gen_cubbez_w_catmull_rom(first, second, third, forth)
                {
                    cubbez.start += offset;
                    cubbez.cp1 += offset;
                    cubbez.cp2 += offset;
                    cubbez.end += offset;

                    // Bounds are definitely inside the polygon of the control points. (Could be improved with the second derivative of the bezier curve)
                    bounds.take_point(na::Point2::<f64>::from(cubbez.start));
                    bounds.take_point(na::Point2::<f64>::from(cubbez.cp1));
                    bounds.take_point(na::Point2::<f64>::from(cubbez.cp2));
                    bounds.take_point(na::Point2::<f64>::from(cubbez.end));

                    bounds.loosen(start_width.max(end_width));
                    // Ceil to nearest integers to avoid subpixel placement errors between stroke elements.
                    bounds = geometry::aabb_ceil(bounds);

                    commands.append(&mut compose::compose_cubbez_variable_width(
                        cubbez,
                        start_width,
                        end_width,
                        true,
                    ));
                } else if let Some(mut line) = curves::gen_line(first, second) {
                    line.start += offset;
                    line.end += offset;

                    commands.append(&mut compose::compose_linear_variable_width(
                        line,
                        start_width,
                        end_width,
                        true,
                    ));
                }
                let path = svg::node::element::Path::new()
                    // avoids gaps between each section
                    .set("stroke", self.brush.color.to_css_color())
                    .set("stroke-width", 1.0)
                    .set("stroke-linejoin", "round")
                    .set("stroke-linecap", "round")
                    .set("fill", self.brush.color.to_css_color())
                    .set("d", path::Data::from(commands));

                match rough_rs::node_to_string(&path) {
                    Ok(mut svg_data) => {
                        if svg_root {

                        svg_data =
                            compose::wrap_svg(&svg_data, Some(bounds), Some(bounds), true, false);
                        }
                        Some(render::Svg { svg_data, bounds })
                    }
                    Err(e) => {
                        log::error!("rough_rs::node_to_string() failed in cubbez_svg_data() of brushstroke, {}", e);
                        None
                    }
                }
            })
            .collect();

        //println!("{}", svg);
        Ok(svgs)
    }

    pub fn experimental_svg_data(
        &self,
        _offset: na::Vector2<f64>,
    ) -> Result<Option<render::Svg>, anyhow::Error> {
        Ok(None)
    }

    pub fn templates_svg_data(
        &self,
        offset: na::Vector2<f64>,
    ) -> Result<Option<render::Svg>, anyhow::Error> {
        if self.elements.len() <= 1 {
            return Ok(None);
        }

        let mut cx = tera::Context::new();

        let color = self.brush.color.to_css_color();
        let width = self.brush.width();
        let sensitivity = self.brush.sensitivity();

        let mut bounds: Vec<p2d::bounding_volume::AABB> =
            Vec::with_capacity(self.elements.len() / 4);
        let mut teraelements: Vec<(TeraElement, TeraElement, TeraElement, TeraElement)> =
            Vec::with_capacity(self.elements.len() / 4);

        self.elements
            .par_iter()
            .zip(self.elements.par_iter().skip(1))
            .zip(self.elements.par_iter().skip(2))
            .zip(self.elements.par_iter().skip(3))
            .map(|(((first, second), third), fourth)| {
                let mut bounds = p2d::bounding_volume::AABB::new_invalid();

                bounds.take_point(na::Point2::<f64>::from(first.inputdata.pos()));
                bounds.take_point(na::Point2::<f64>::from(second.inputdata.pos()));
                bounds.take_point(na::Point2::<f64>::from(third.inputdata.pos()));
                bounds.take_point(na::Point2::<f64>::from(fourth.inputdata.pos()));

                bounds.loosen(Brush::TEMPLATE_BOUNDS_PADDING);
                // Ceil to nearest integers to avoid subpixel placement errors between stroke elements.
                bounds = geometry::aabb_ceil(bounds);

                (
                    bounds,
                    (
                        TeraElement {
                            pressure: first.inputdata.pressure(),
                            x: first.inputdata.pos()[0] + offset[0],
                            y: first.inputdata.pos()[1] + offset[1],
                        },
                        TeraElement {
                            pressure: second.inputdata.pressure(),
                            x: second.inputdata.pos()[0] + offset[0],
                            y: second.inputdata.pos()[1] + offset[1],
                        },
                        TeraElement {
                            pressure: third.inputdata.pressure(),
                            x: third.inputdata.pos()[0] + offset[0],
                            y: third.inputdata.pos()[1] + offset[1],
                        },
                        TeraElement {
                            pressure: fourth.inputdata.pressure(),
                            x: fourth.inputdata.pos()[0] + offset[0],
                            y: fourth.inputdata.pos()[1] + offset[1],
                        },
                    ),
                )
            })
            .unzip_into_vecs(&mut bounds, &mut teraelements);

        let bounds = bounds.iter().fold(
            p2d::bounding_volume::AABB::new_invalid(),
            |first, second| first.merged(second),
        );

        cx.insert("color", &color);
        cx.insert("width", &width);
        cx.insert("sensitivity", &sensitivity);
        cx.insert("attributes", "");
        cx.insert("elements", &teraelements);

        if let brush::BrushStyle::CustomTemplate(templ) = &self.brush.current_style {
            let svg_data = tera::Tera::one_off(templ.as_str(), &cx, false)?;
            return Ok(Some(render::Svg { svg_data, bounds }));
        } else {
            return Err(anyhow::anyhow!(
                "template_svg_data() called, but brush is not BrushStyle::CustomTemplate"
            ));
        };
    }
}
