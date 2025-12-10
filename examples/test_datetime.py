import datetime

def test_datetime_constants():
    """Test datetime module constants."""
    minyear = datetime.MINYEAR
    maxyear = datetime.MAXYEAR
    return maxyear

def test_datetime_now():
    """Test datetime.datetime.now()."""
    now = datetime.datetime.now()
    return now

def test_date_today():
    """Test datetime.date.today()."""
    today = datetime.date.today()
    return today

def test_datetime_constructor():
    """Test datetime constructor."""
    dt = datetime.datetime(2024, 12, 10, 14, 30, 0)
    return dt

def test_date_constructor():
    """Test date constructor."""
    d = datetime.date(2024, 12, 10)
    return d

def test_time_constructor():
    """Test time constructor."""
    t = datetime.time(14, 30, 0)
    return t

def test_timedelta():
    """Test timedelta constructor."""
    td = datetime.timedelta(1, 3600, 0)
    return td

def test_datetime_add_timedelta():
    """Test datetime + timedelta arithmetic."""
    dt = datetime.datetime(2024, 12, 10, 14, 30, 0)
    td = datetime.timedelta(1, 0, 0)
    result = dt + td
    return result

def test_datetime_sub_timedelta():
    """Test datetime - timedelta arithmetic."""
    dt = datetime.datetime(2024, 12, 10, 14, 30, 0)
    td = datetime.timedelta(1, 0, 0)
    result = dt - td
    return result

def test_datetime_diff():
    """Test datetime - datetime arithmetic."""
    dt1 = datetime.datetime(2024, 12, 10, 14, 30, 0)
    dt2 = datetime.datetime(2024, 12, 9, 14, 30, 0)
    diff = dt1 - dt2
    return diff

def test_date_add_timedelta():
    """Test date + timedelta arithmetic."""
    d = datetime.date(2024, 12, 10)
    td = datetime.timedelta(7, 0, 0)
    result = d + td
    return result

def test_date_diff():
    """Test date - date arithmetic."""
    d1 = datetime.date(2024, 12, 10)
    d2 = datetime.date(2024, 12, 1)
    diff = d1 - d2
    return diff

def test_timedelta_add():
    """Test timedelta + timedelta arithmetic."""
    td1 = datetime.timedelta(1, 0, 0)
    td2 = datetime.timedelta(2, 0, 0)
    result = td1 + td2
    return result

def test_timedelta_sub():
    """Test timedelta - timedelta arithmetic."""
    td1 = datetime.timedelta(5, 0, 0)
    td2 = datetime.timedelta(2, 0, 0)
    result = td1 - td2
    return result
