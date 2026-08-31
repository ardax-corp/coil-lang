class Cell {
    pub n: int,
    hidden: int,
}

impl Cell {
    pub fn get() -> int {
        return self.n + self.hidden;
    }

    fn bump() {
        self.hidden = self.hidden + 1;
    }

    pub fn tick() -> int {
        self.bump();
        return self.get();
    }
}

test("pub field and method from outside") {
    let c = new Cell(3, 4);
    assert(c.n == 3)?;
    assert(c.get() == 7)?;
    assert(c.tick() == 8)?;
}
