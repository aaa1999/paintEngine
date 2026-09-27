/// 轴对齐矩形。坐标含义随上下文：屏幕物理像素或画布像素。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    /// 右边界（不含），i64 防止 i32 溢出。
    pub fn x2(&self) -> i64 {
        self.x as i64 + self.w as i64
    }

    pub fn y2(&self) -> i64 {
        self.y as i64 + self.h as i64
    }

    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    pub fn intersect(&self, o: &Rect) -> Option<Rect> {
        let x = self.x.max(o.x) as i64;
        let y = self.y.max(o.y) as i64;
        let x2 = self.x2().min(o.x2());
        let y2 = self.y2().min(o.y2());
        if x2 <= x || y2 <= y {
            None
        } else {
            Some(Rect::new(
                x as i32,
                y as i32,
                (x2 - x) as u32,
                (y2 - y) as u32,
            ))
        }
    }

    pub fn union(&self, o: &Rect) -> Rect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        let x = self.x.min(o.x) as i64;
        let y = self.y.min(o.y) as i64;
        let x2 = self.x2().max(o.x2());
        let y2 = self.y2().max(o.y2());
        Rect::new(x as i32, y as i32, (x2 - x) as u32, (y2 - y) as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intersect_and_union() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 10, 10);
        assert_eq!(a.intersect(&b), Some(Rect::new(5, 5, 5, 5)));
        assert_eq!(a.union(&b), Rect::new(0, 0, 15, 15));

        let c = Rect::new(20, 20, 5, 5);
        assert_eq!(a.intersect(&c), None);
        assert_eq!(a.union(&c), Rect::new(0, 0, 25, 25));
    }

    #[test]
    fn negative_coords() {
        let a = Rect::new(-10, -10, 5, 5);
        assert_eq!(a.x2(), -5);
        let b = Rect::new(-3, -3, 10, 10);
        assert_eq!(a.intersect(&b), None);
        let c = Rect::new(-7, -7, 10, 10);
        assert_eq!(a.intersect(&c), Some(Rect::new(-7, -7, 2, 2)));
    }
}
